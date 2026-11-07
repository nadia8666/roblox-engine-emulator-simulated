//! Source: a module's lines with their heat in the metric and, beside it, every kind's figure for the line:
//! instructions, allocations, Luau cache misses; the selected line's opcodes below. `s` lists the lines by heat.

use std::{collections::BTreeMap, rc::Rc};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::{
    kit::{self, Item, col, col_fill, col_r},
    overview::ranked,
};
use crate::{
    app::{App, Focus, View},
    fmt,
    keys::Key,
    model::{LineFig, Source},
    theme::{Mark, Theme},
};

/// A row of the listing: a line (its text, its figures summed over the functions on it, the innermost function that
/// holds it), or a gap in a partial listing (line -1)
#[derive(Clone, Debug)]
pub struct SrcRow {
    pub line: i64,
    pub text: String,
    pub fig: Option<LineFig>,
    pub func: Option<u32>,
}

fn add(into: &mut LineFig, fig: &LineFig) {
    into.work += fig.work;
    into.total_work += fig.total_work;
    into.insns += fig.insns;
    into.total_insns += fig.total_insns;
    into.time += fig.time;
    into.total_time += fig.total_time;
    into.alloc += fig.alloc;
    into.allocs += fig.allocs;
    into.misses += fig.misses;
    into.cache_runs += fig.cache_runs;
    if into.counters.len() < fig.counters.len() {
        into.counters.resize(fig.counters.len(), 0.0);
    }
    for (slot, value) in into.counters.iter_mut().zip(&fig.counters) {
        *slot += value;
    }
    for (op, count) in &fig.ops {
        *into.ops.entry(op.clone()).or_default() += count;
    }
}

/// The module shown: the followed function's, else the one shown last, else the hottest function's
pub fn module(app: &App) -> Option<String> {
    let p = &app.profiles[app.shown];
    if let Some(focus) = app.focus {
        let module = &p.funcs[focus.func as usize].module;
        if !module.is_empty() {
            return Some(module.clone());
        }
    }
    if let Some(module) = app.source.module.as_ref().filter(|m| !m.is_empty()) {
        return Some(module.clone());
    }
    let func = ranked(app).into_iter().map(|r| r.func).find(|f| !p.funcs[*f as usize].module.is_empty())?;
    Some(p.funcs[func as usize].module.clone())
}

/// The listing's rows, in file order
pub fn rows(app: &App, module: &str) -> (Rc<Option<Source>>, Vec<SrcRow>) {
    let p = &app.profiles[app.shown];
    let source = app.source_text(module);
    let mut figs: BTreeMap<i64, LineFig> = BTreeMap::new();
    for (line, _, fig) in p.module_lines(module) {
        add(figs.entry(line).or_default(), fig);
    }
    // the innermost function holding each line
    let funcs: Vec<(u32, i64, i64)> = p.funcs.iter().enumerate().filter(|(_, f)| f.module == module).map(|(i, f)| (i as u32, f.line, f.last_line.max(f.line))).collect();
    let holder = |line: i64| funcs.iter().filter(|(_, from, to)| *from <= line && line <= *to).max_by_key(|(_, from, _)| *from).map(|(f, _, _)| *f);
    let mut numbers: Vec<i64> = figs.keys().copied().collect();
    let whole = source.as_ref().as_ref().is_some_and(|s| s.whole);
    if let Some(s) = source.as_ref() {
        numbers.extend(s.lines.keys().copied());
    }
    numbers.sort();
    numbers.dedup();
    if whole {
        let last = numbers.last().copied().unwrap_or(0);
        numbers = (1..=last).collect();
    }
    let mut out = Vec::with_capacity(numbers.len());
    let mut previous: Option<i64> = None;
    for line in numbers {
        if line <= 0 {
            continue;
        }
        if let Some(prev) = previous {
            if line > prev + 1 && !whole {
                out.push(SrcRow { line: -1, text: "⋯".into(), fig: None, func: None });
            }
        }
        previous = Some(line);
        let text = source.as_ref().as_ref().and_then(|s| s.lines.get(&line).cloned()).unwrap_or_default();
        out.push(SrcRow { line, text, fig: figs.get(&line).cloned(), func: holder(line) });
    }
    (source, out)
}

/// A path's last three parts (`demo/src/Grid.luau`): where the file is, without the machine's folders
pub fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    let from = parts.len().saturating_sub(3);
    parts[from..].join("/")
}

/// The smallest figure a cell shows (below it, the cell stays empty rather than read 0)
fn shown_floor(unit: crate::metric::Unit) -> f64 {
    match unit {
        crate::metric::Unit::Ms => 5e-7,
        _ => 0.5,
    }
}

/// The rows as shown: in file order, or by heat (`s`)
fn shown(app: &App, rows: &[SrcRow]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    if app.source.hot {
        let metric = app.metric;
        order.retain(|&i| rows[i].fig.as_ref().is_some_and(|f| metric.line_self(f) > 0.0 || metric.line_total(f) > 0.0));
        order.sort_by(|&a, &b| {
            let va = rows[a].fig.as_ref().map_or(0.0, |f| metric.line_self(f));
            let vb = rows[b].fig.as_ref().map_or(0.0, |f| metric.line_self(f));
            vb.total_cmp(&va).then(a.cmp(&b))
        });
    }
    order
}

/// Luau's keywords, which the listing shows in the accent
const KEYWORDS: [&str; 22] = ["and", "break", "continue", "do", "else", "elseif", "end", "export", "false", "for", "function", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until"];

/// A line of Luau coloured as an editor would: keywords in the accent, strings green, comments muted
pub fn highlight(code: &str, theme: &Theme) -> Line<'static> {
    highlight_with(code, theme, false)
}

/// A line of Luau coloured, or all muted (a line that never ran)
pub fn highlight_with(code: &str, theme: &Theme, dim: bool) -> Line<'static> {
    let code = fmt::cells(code);
    let ink = if dim { theme.muted } else { theme.ink };
    let keyword = if dim { Style::new().fg(theme.muted) } else { Style::new().fg(theme.accent) };
    let string = if dim { Style::new().fg(theme.muted) } else { Style::new().fg(theme.good) };
    let comment = Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC);
    let chars: Vec<char> = code.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut i = 0;
    let mut plain = String::new();
    let flush = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), Style::new().fg(ink)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            flush(&mut plain, &mut spans);
            spans.push(Span::styled(chars[i..].iter().collect::<String>(), comment));
            break;
        }
        if c == '"' || c == '\'' || c == '`' {
            flush(&mut plain, &mut spans);
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(chars.len());
            spans.push(Span::styled(chars[i..end].iter().collect::<String>(), string));
            i = end;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            if KEYWORDS.contains(&word.as_str()) {
                flush(&mut plain, &mut spans);
                spans.push(Span::styled(word, keyword));
            } else {
                plain.push_str(&word);
            }
            i = j;
            continue;
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut spans);
    Line::from(spans)
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let Some(module) = module(app) else {
        let inner = kit::draw_panel(buf, area, kit::panel(theme, "Source", "", false));
        kit::empty(buf, inner, theme, "no Luau source in this profile (work, time and alloc profiles keep it)");
        return;
    };
    if app.source.module.as_deref() != Some(module.as_str()) {
        // (another module: its line is the followed one's, else its hottest)
        app.source.module = Some(module.clone());
        app.source.line = app.focus.and_then(|f| f.line).unwrap_or(-1);
        app.source.list = Default::default();
    }
    let (source, rows) = rows(app, &module);
    let order = shown(app, &rows);
    // the selected line: the view's, or the focus's, or the hottest
    let metric = app.metric;
    let mut selected = order.iter().position(|&i| rows[i].line == app.source.line);
    if selected.is_none() {
        selected = order.iter().enumerate().filter(|(_, i)| rows[**i].fig.is_some()).max_by(|a, b| {
            let va = rows[*a.1].fig.as_ref().map_or(0.0, |f| metric.line_self(f));
            let vb = rows[*b.1].fig.as_ref().map_or(0.0, |f| metric.line_self(f));
            va.total_cmp(&vb).then(b.0.cmp(&a.0))
        }).map(|(at, _)| at);
    }
    let detail_height = 5u16;
    let parts = kit::rows(area, &[area.height.saturating_sub(detail_height), detail_height]);
    // (the listing's rows: its panel's inside less the header)
    let list_height = parts[0].height.saturating_sub(3) as usize;
    let mut list = app.source.list;
    list.selected = selected.unwrap_or(0);
    // (a line reached from elsewhere, off the screen: shown a third of the way down, its context above it)
    if list.selected + 1 < list.offset || list.selected > list.offset + list_height.max(1) {
        list.offset = list.selected.saturating_sub(list_height / 3);
    }
    list.fit(order.len(), list_height);
    if let Some(&row) = order.get(list.selected) {
        app.source.line = rows[row].line;
    }
    let p = &app.profiles[app.shown];
    let total = metric.total(p);
    let unit = metric.unit();
    let label = metric.short_label(p);
    let note = match source.as_ref() {
        Some(s) if s.changed => format!("{} changed since the run: the profile's lines", short_path(&s.path)),
        Some(s) if s.whole => short_path(&s.path),
        Some(s) => format!("{}: not here, the profile's lines", short_path(&s.path)),
        None => "no source text: figures only".to_string(),
    };
    let scroll = if order.len() > list_height { format!("{}-{} of {}", fmt::grouped((list.offset + 1) as f64), fmt::grouped((list.offset + list_height).min(order.len()) as f64), fmt::grouped(order.len() as f64)) } else { String::new() };
    let block = kit::bottom(kit::note_right(kit::panel(theme, crate::model::short_module(&module), &note, true), theme, &scroll), super::legend(app, theme, area.width));
    let inner = kit::draw_panel(buf, parts[0], block);
    // columns: line, the metric's own and total, the heat bar, then the other kinds the profile has, then the code
    let has_total = matches!(metric, crate::metric::Metric::Work | crate::metric::Metric::Insns | crate::metric::Metric::Time);
    let mut cols = vec![col_r("line", 5), col_r(&fmt::fit(&label, 10), 10)];
    if has_total {
        cols.push(col_r("with calls", 10));
    }
    let bar_col = cols.len();
    cols.push(col("", 8));
    let wide = area.width >= 120;
    let extra_insns = wide && metric != crate::metric::Metric::Insns && p.raw.work.is_some();
    let extra_alloc = wide && !matches!(metric, crate::metric::Metric::AllocBytes | crate::metric::Metric::AllocCount) && p.raw.alloc.is_some();
    let extra_miss = wide && metric != crate::metric::Metric::CacheMisses && p.raw.luau_caches.is_some();
    if extra_insns {
        cols.push(col_r("insns", 7));
    }
    if extra_alloc {
        cols.push(col_r("alloc", 9));
    }
    if extra_miss {
        cols.push(col_r("misses", 7));
    }
    cols.push(col("", 1));
    cols.push(col_fill("code", 1));
    let max = rows.iter().filter_map(|r| r.fig.as_ref()).map(|f| metric.line_self(f)).fold(0.0, f64::max);
    let focus_func = app.focus.map(|f| f.func);
    let query = app.query.to_lowercase();
    let items: Vec<Vec<Item>> = order
        .iter()
        .map(|&row_index| {
            let row = &rows[row_index];
            let mut cells: Vec<Item> = Vec::with_capacity(cols.len());
            if row.line < 0 {
                cells.push(Item::text("⋯", Style::new().fg(theme.muted)));
                return cells;
            }
            let own = row.fig.as_ref().map_or(0.0, |f| metric.line_self(f));
            let share = if total > 0.0 { own / total } else { 0.0 };
            let style = theme.row(share);
            cells.push(Item::text(row.line.to_string(), Style::new().fg(theme.muted)));
            match &row.fig {
                Some(fig) => {
                    cells.push(if own >= shown_floor(unit) { Item::text(fmt::short(unit, own), style) } else { Item::empty() });
                    if has_total {
                        let with = metric.line_total(fig);
                        cells.push(if with > own { Item::text(fmt::short(unit, with), Style::new().fg(theme.ink2)) } else { Item::empty() });
                    }
                    cells.push(Item::Spans(kit::bar(if max > 0.0 { own / max } else { 0.0 }, 8, theme.heat_of(share))));
                    if extra_insns {
                        cells.push(if fig.insns > 0.0 { Item::text(fmt::compact(fig.insns), Style::new().fg(theme.ink2)) } else { Item::empty() });
                    }
                    if extra_alloc {
                        cells.push(if fig.alloc > 0.0 { Item::text(fmt::bytes(fig.alloc), Style::new().fg(theme.ink2)) } else { Item::empty() });
                    }
                    if extra_miss {
                        cells.push(if fig.misses > 0.0 { Item::text(fmt::compact(fig.misses), Style::new().fg(theme.serious)) } else { Item::empty() });
                    }
                }
                None => {
                    for _ in 0..(bar_col - 1 + 1 + extra_insns as usize + extra_alloc as usize + extra_miss as usize) {
                        cells.push(Item::empty());
                    }
                }
            }
            // the followed function's lines get a gutter mark
            cells.push(if row.func.is_some() && row.func == focus_func { Item::Spans(Line::from(Span::styled("│", Style::new().fg(theme.accent)))) } else { Item::empty() });
            let found = !query.is_empty() && row.text.to_lowercase().contains(&query);
            cells.push(if found { Item::Spans(Line::from(Span::styled(fmt::cells(&row.text), Style::new().fg(theme.found_ink).bg(theme.found)))) } else { Item::Spans(highlight_with(&row.text, theme, row.fig.is_none())) });
            cells
        })
        .collect();
    let grid = kit::table(buf, inner, theme, &cols, items, Some(list.selected), list.offset);
    kit::mark(buf, grid.column(bar_col), Mark::Bar);
    list.offset = grid.offset;
    app.source.list = list;
    // the selected line in full
    let Some(row) = order.get(list.selected).map(|&i| &rows[i]) else { return };
    let func_label = row.func.map(|f| p.funcs[f as usize].label()).unwrap_or_default();
    let inner = kit::draw_panel(buf, parts[1], kit::panel(theme, &format!("Line {}", row.line.max(0)), &func_label, false));
    let Some(fig) = &row.fig else {
        kit::empty(buf, inner, theme, "nothing ran here");
        return;
    };
    let figure = |label: &str, value: String| vec![Span::styled(format!("{label} "), Style::new().fg(theme.muted)), Span::styled(value, Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)), Span::raw("    ")];
    let mut spans: Vec<Span> = Vec::new();
    if p.raw.work.is_some() {
        spans.extend(figure("work", fmt::ms(fig.work)));
        if fig.total_work > fig.work + 1e-9 {
            spans.extend(figure("with calls", fmt::ms(fig.total_work)));
        }
        spans.extend(figure("instructions", fmt::grouped(fig.insns)));
    }
    if p.raw.time.is_some() {
        spans.extend(figure("host", fmt::ms(fig.time)));
    }
    if p.raw.alloc.is_some() && fig.allocs > 0.0 {
        spans.extend(figure("allocated", format!("{}, {} objects{}", fmt::bytes(fig.alloc), fmt::grouped(fig.allocs), if p.line_alloc_complete { "" } else { " (kept stacks only)" })));
    }
    if fig.cache_runs > 0.0 {
        spans.extend(figure("cache misses", format!("{} of {}", fmt::grouped(fig.misses), fmt::grouped(fig.cache_runs))));
    }
    if let Some(c) = &p.raw.counters {
        for (name, value) in c.names.iter().zip(&fig.counters) {
            if *value > 0.0 {
                spans.extend(figure(name, fmt::compact(*value)));
            }
        }
    }
    let mut lines = vec![Line::from(spans)];
    let mut ops: Vec<(&String, &u64)> = fig.ops.iter().collect();
    ops.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    if !ops.is_empty() {
        let mut spans = vec![Span::styled("opcodes ", Style::new().fg(theme.muted))];
        for (index, (op, n)) in ops.iter().take(8).enumerate() {
            if index > 0 {
                spans.push(Span::styled(" · ", Style::new().fg(theme.faint)));
            }
            spans.push(Span::styled(op.to_string(), Style::new().fg(theme.ink2)));
            spans.push(Span::styled(format!(" {}", fmt::grouped(**n as f64)), Style::new().fg(theme.ink)));
        }
        lines.push(Line::from(spans));
    }
    let mut code = vec![Span::styled("code ", Style::new().fg(theme.muted))];
    code.extend(highlight(row.text.trim(), theme).spans);
    lines.push(Line::from(code));
    Paragraph::new(lines).render(inner, buf);
}

pub fn key(app: &mut App, key: Key) {
    let Some(module) = module(app) else { return };
    let (_, rows) = rows(app, &module);
    let order = shown(app, &rows);
    let mut list = app.source.list;
    list.selected = order.iter().position(|&i| rows[i].line == app.source.line).unwrap_or(list.selected);
    if list.key(key, order.len(), app.page) {
        if let Some(&i) = order.get(list.selected) {
            app.source.line = rows[i].line;
            if let Some(func) = rows[i].func {
                app.focus = Some(Focus { func, line: Some(rows[i].line) });
            }
        }
        app.source.list = list;
        return;
    }
    match key {
        Key::Left | Key::Right => {
            // the previous or next line that is not negligible
            let metric = app.metric;
            let p = &app.profiles[app.shown];
            let total = metric.total(p);
            let hot = |i: usize| rows[order[i]].fig.as_ref().is_some_and(|f| total > 0.0 && metric.line_self(f) / total >= crate::theme::HEAT_FROM);
            if let Some(at) = super::next_match(order.len(), list.selected, key == Key::Right, false, hot) {
                app.source.line = rows[order[at]].line;
                if let Some(func) = rows[order[at]].func {
                    app.focus = Some(Focus { func, line: Some(rows[order[at]].line) });
                }
            }
        }
        Key::Char('s') => {
            app.source.hot = !app.source.hot;
            app.status = if app.source.hot { "lines by heat".into() } else { "lines in file order".into() };
        }
        Key::Enter => {
            if let Some(func) = order.get(list.selected).and_then(|&i| rows[i].func) {
                app.focus = Some(Focus { func, line: None });
                app.set_view(View::Calls);
            }
        }
        _ => {}
    }
}

pub fn copy(app: &App) -> Option<String> {
    let module = module(app)?;
    let p = &app.profiles[app.shown];
    Some(match p.module_path(&module) {
        Some(path) => format!("{path}:{}", app.source.line),
        None => format!("{module}:{}", app.source.line),
    })
}

pub fn find(app: &mut App, query: &str, forward: bool, from_start: bool) -> bool {
    let Some(module) = module(app) else { return false };
    let (_, rows) = rows(app, &module);
    let order = shown(app, &rows);
    let at = order.iter().position(|&i| rows[i].line == app.source.line).unwrap_or(0);
    match super::next_match(order.len(), at, forward, from_start, |i| rows[order[i]].text.to_lowercase().contains(query)) {
        Some(found) => {
            app.source.line = rows[order[found]].line;
            true
        }
        None => false,
    }
}

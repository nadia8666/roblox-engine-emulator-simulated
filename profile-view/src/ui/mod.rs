//! The screen: a header bar (the run, the metric and its total), the views' tabs, the view's panels, and a footer
//! with the view's keys as chips. `?` lays the help over it.

pub mod alloc;
pub mod caches;
pub mod calls;
pub mod compare;
pub mod diff;
pub mod flame;
pub mod heap;
pub mod issues;
pub mod kit;
pub mod overview;

pub mod source;
pub mod timeline;

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Padding, Paragraph, Tabs, Widget},
};

use crate::{
    app::{App, View},
    fmt,
    keys::Key,
    theme::Theme,
};

/// The smallest screen the views lay out in
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 16;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    draw_buffer(frame.buffer_mut(), area, app);
}

/// Draws the whole screen into a buffer
pub fn draw_buffer(buf: &mut Buffer, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    Block::new().style(Style::new().bg(theme.page).fg(theme.ink)).render(area, buf);
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        Paragraph::new(format!("ree profile view needs {MIN_WIDTH}x{MIN_HEIGHT} at least ({}x{} now)", area.width, area.height)).render(area, buf);
        return;
    }
    header(buf, Rect { height: 1, ..area }, app, &theme);
    tabs(buf, Rect { y: area.y + 1, height: 1, ..area }, app, &theme);
    let footer_y = area.bottom() - 1;
    let body = Rect { x: area.x, y: area.y + 2, width: area.width, height: footer_y.saturating_sub(area.y + 2) };
    app.page = body.height.saturating_sub(6).max(1) as usize;
    match app.view {
        View::Overview => overview::draw(buf, body, app, &theme),
        View::Flame => flame::draw(buf, body, app, &theme),
        View::Calls => calls::draw(buf, body, app, &theme),
        View::Source => source::draw(buf, body, app, &theme),
        View::Alloc => alloc::draw(buf, body, app, &theme),
        View::Heap => heap::draw(buf, body, app, &theme),
        View::Timeline => timeline::draw(buf, body, app, &theme),
        View::Caches => caches::draw(buf, body, app, &theme),
        View::Issues => issues::draw(buf, body, app, &theme),
        View::Diff => diff::draw(buf, body, app, &theme),
        View::Compare => compare::draw(buf, body, app, &theme),
    }
    footer(buf, Rect { y: footer_y, height: 1, ..area }, app, &theme);
    if app.help {
        help(buf, area, app, &theme);
    }
}

/// The header bar: the brand, what ran, and the metric's total at the right
fn header(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let bar = Style::new().bg(theme.raised).fg(theme.ink);
    Block::new().style(bar).render(area, buf);
    let mut spans = vec![
        Span::styled(" ◆ ree profile ", Style::new().fg(theme.accent_ink).bg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled("  ", bar),
    ];
    let mut right: Vec<Span> = Vec::new();
    if app.has_profile() {
        let p = &app.profiles[app.shown];
        let run = &p.raw.run;
        let cut_short_at = p.raw.counters.as_ref().and_then(|counters| counters.cut_short);
        if let Some(seconds) = cut_short_at {
            let warning_style = Style::new().fg(theme.critical).bg(theme.raised).add_modifier(Modifier::BOLD);
            spans.push(Span::styled(format!(" HW CUT SHORT at {seconds:.3} s  "), warning_style));
        }
        if app.profiles.len() == 2 {
            spans.push(Span::styled(format!("{}  ", app.side_name(app.shown)), bar.add_modifier(Modifier::BOLD)));
        }
        // (✗ only for a run that failed or timed out: a harness still waiting when the run ends, `yielded`, is a run's
        // usual end)
        let failed = matches!(run.status.as_str(), "error" | "failed") || run.status.contains("timeout") || run.status.contains("timed out");
        let (icon, color) = if run.status == "ok" {
            ("✓ ", theme.good)
        } else if failed {
            ("✗ ", theme.critical)
        } else {
            ("○ ", theme.muted)
        };
        spans.push(Span::styled(icon, Style::new().fg(color).bg(theme.raised)));
        let mut what = vec![format!("{} {}", run.command, run.status)];
        if let Some(harness) = p.harness() {
            what.push(harness);
        }
        what.push(format!("{} frames", fmt::grouped(run.frames as f64)));
        what.push(format!("{:.2} s", run.seconds));
        spans.push(Span::styled(what.join(" · "), Style::new().fg(theme.ink2).bg(theme.raised)));
        let metric = app.metric;
        right.push(Span::styled(format!("{} ", metric.label(p)), Style::new().fg(theme.muted).bg(theme.raised)));
        // (a metric the profile's kinds did not measure is said so, never shown as 0; the timeline's frames still carry
        // the work clock without the work kind)
        let frames_work = p.raw.timeline.as_ref().and_then(|t| t.values("work_ms")).map(|values| values.iter().sum::<f64>());
        let total = if crate::metric::Metric::available(p).contains(&metric) {
            fmt::value(metric.unit(), metric.total(p))
        } else if let Some(work) = frames_work.filter(|_| metric == crate::metric::Metric::Work) {
            format!("{} in frames", fmt::ms(work))
        } else {
            "not measured".into()
        };
        right.push(Span::styled(format!("{total} "), bar.add_modifier(Modifier::BOLD)));
    } else if let Some(compare) = &app.compare {
        spans.push(Span::styled(compare.title.clone(), Style::new().fg(theme.ink2).bg(theme.raised)));
    }
    if app.scripts_only {
        right.insert(0, Span::styled(" scripts only ", Style::new().fg(theme.accent).bg(theme.raised).add_modifier(Modifier::BOLD)));
        right.insert(1, Span::styled("  ", bar));
    }
    let right_width: u16 = right.iter().map(|s| s.width() as u16).sum();
    Paragraph::new(Line::from(spans)).style(bar).render(Rect { width: area.width.saturating_sub(right_width + 1), ..area }, buf);
    Paragraph::new(Line::from(right)).style(bar).alignment(Alignment::Right).render(area.intersection(Rect { x: area.right().saturating_sub(right_width), width: right_width, ..area }), buf);
}

/// The views' tabs: the active one in the accent, the ones the input lacks muted
fn tabs(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    // (narrow screens: no gaps between the tabs, and Allocations as Alloc)
    let titles = |compact: bool| -> Vec<Line> {
        View::ALL
            .iter()
            .map(|&view| {
                let style = if app.available(view) { Style::new().fg(theme.ink2) } else { Style::new().fg(theme.faint) };
                let title = if compact && view == View::Alloc { "Alloc" } else { view.title() };
                Line::from(vec![Span::styled(format!(" {} ", view.key()), style.add_modifier(Modifier::BOLD)), Span::styled(format!("{title} "), style)])
            })
            .collect()
    };
    let wide: usize = titles(false).iter().map(|t| t.width() + 1).sum();
    let compact = wide + 1 > area.width as usize;
    let selected = View::ALL.iter().position(|&v| v == app.view);
    Tabs::new(titles(compact))
        .select(selected)
        .style(Style::new().bg(theme.page))
        .highlight_style(Style::new().fg(theme.accent_ink).bg(theme.accent).add_modifier(Modifier::BOLD))
        .divider(Span::styled(if compact { "" } else { " " }, Style::new().bg(theme.page)))
        .padding("", "")
        .render(Rect { x: area.x + 1, width: area.width.saturating_sub(1), ..area }, buf);
}

/// What a view's heat is a share of
pub fn heat_subject(app: &App) -> String {
    let p = &app.profiles[app.shown];
    match app.view {
        View::Alloc if app.metric == crate::metric::Metric::AllocCount => "allocations".into(),
        View::Alloc => "allocated bytes".into(),
        View::Caches => "cache misses".into(),
        _ => app.metric.label(p),
    }
}

/// The heat scale's legend for a view's panel (its bottom border)
pub fn legend(app: &App, theme: &Theme, width: u16) -> Line<'static> {
    if !app.has_profile() {
        return Line::default();
    }
    kit::heat_legend(theme, &heat_subject(app), width / 4)
}

/// The keys a view takes, for the footer
fn view_keys(view: View) -> &'static [(&'static str, &'static str)] {
    match view {
        View::Overview => &[("↑↓", "issue"), ("enter", "open")],
        View::Flame => &[("←→↑↓", "move"), ("enter", "zoom"), ("esc", "out"), ("s", "order")],
        View::Calls => &[("↑↓", "move"), ("enter", "centre"), ("esc", "back"), ("s", "sort")],
        View::Source => &[("↑↓", "line"), ("←→", "hot line"), ("s", "by heat"), ("enter", "calls")],
        View::Alloc => &[("↑↓", "site"), ("s", "sort"), ("enter", "source")],
        View::Heap => &[("←→", "snapshot"), ("tab", "pane"), ("↑↓", "row"), ("enter", "source")],
        View::Timeline => &[("←→", "frame"), ("pgup/pgdn", "±10"), ("enter", "zoom"), ("esc", "out"), ("[ ]", "series")],
        View::Caches => &[("↑↓", "site"), ("s", "sort"), ("enter", "source")],
        View::Issues => &[("↑↓", "issue"), ("enter", "open")],
        View::Diff => &[("tab", "table"), ("↑↓", "row"), ("s", "sort"), ("enter", "open"), ("x", "side")],
        View::Compare => &[("↑↓", "variant"), ("←→", "size"), ("enter", "open"), ("D", "diff")],
    }
}

const GLOBAL_KEYS: [(&str, &str); 7] = [("m", "metric"), ("f", "scripts"), ("/", "find"), ("y", "copy"), ("t", "theme"), ("?", "help"), ("q", "quit")];

fn footer(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let bar = Style::new().bg(theme.raised).fg(theme.ink2);
    Block::new().style(bar).render(area, buf);
    if app.searching {
        let line = Line::from(vec![
            Span::styled(" / ", Style::new().fg(theme.accent_ink).bg(theme.accent).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}▏", app.query), Style::new().fg(theme.ink).bg(theme.raised)),
            Span::styled("   ", bar),
        ]);
        let mut spans = line.spans;
        spans.extend(kit::chips(theme, &[("enter", "find"), ("esc", "cancel")], theme.raised));
        Paragraph::new(Line::from(spans)).style(bar).render(area, buf);
        return;
    }
    let mut spans = vec![Span::styled(" ", bar)];
    spans.extend(kit::chips(theme, view_keys(app.view), theme.raised));
    spans.push(Span::styled("   │   ", Style::new().fg(theme.faint).bg(theme.raised)));
    spans.extend(kit::chips(theme, &GLOBAL_KEYS, theme.raised));
    let status = if app.status.is_empty() { String::new() } else { format!(" {} ", app.status) };
    let status_width = (status.chars().count() as u16).min(area.width / 2);
    Paragraph::new(Line::from(spans)).style(bar).render(Rect { width: area.width.saturating_sub(status_width + 1), ..area }, buf);
    if status_width > 0 {
        let at = Rect { x: area.right() - status_width, width: status_width, ..area };
        Paragraph::new(Line::from(Span::styled(fmt::fit(&status, status_width as usize), Style::new().fg(theme.ink).bg(theme.raised).add_modifier(Modifier::BOLD)))).render(at, buf);
    }
}

fn help(buf: &mut Buffer, area: Rect, app: &App, theme: &Theme) {
    let width = area.width.min(108).saturating_sub(4);
    let height = area.height.saturating_sub(4).min(40);
    let boxed = Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height };
    Clear.render(boxed, buf);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .style(Style::new().bg(theme.raised).fg(theme.ink))
        .title(Line::from(Span::styled(" ? Help ", Style::new().fg(theme.accent).add_modifier(Modifier::BOLD))))
        .title_bottom(Line::from(Span::styled(" any key closes ", Style::new().fg(theme.muted))).right_aligned())
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(boxed);
    block.render(boxed, buf);
    let heading = Style::new().fg(theme.accent).add_modifier(Modifier::BOLD);
    let text = Style::new().fg(theme.ink);
    let dim = Style::new().fg(theme.ink2);
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("Keys", heading)),
        Line::from(kit::chips(theme, &[("1-9", "views"), ("d", "Diff (two profiles)"), ("c", "Compare (a what-if folder)")], theme.raised)),
        Line::from(kit::chips(theme, &[("arrows, hjkl", "move"), ("enter", "in"), ("esc, backspace", "out")], theme.raised)),
        Line::from(kit::chips(theme, &[("m", "metric"), ("s", "sort"), ("f", "scripts only"), ("/", "search"), ("n N", "next")], theme.raised)),
        Line::from(kit::chips(theme, &[("y", "copy"), ("x", "before/after"), ("t", "theme"), ("q", "quit")], theme.raised)),
        Line::from({
            let mut spans = vec![Span::styled("here: ", dim)];
            spans.extend(kit::chips(theme, view_keys(app.view), theme.raised));
            spans
        }),
        Line::default(),
        Line::from(Span::styled("Colour", heading)),
        Line::from(Span::styled("heat: a figure's share of the run, from purple (0.1%) through red and orange to gold (30% and more); grey under 0.1%", text)),
        Line::from(Span::styled("diff: red ▲ more, blue ▼ less · issues: ■ critical ▲ serious ● warning ○ info · variants and phases: a colour each", text)),
        Line::default(),
    ];
    if app.has_profile() {
        let p = &app.profiles[app.shown];
        lines.push(Line::from(Span::styled("Metrics", heading)));
        for metric in app.metrics() {
            let current = metric == app.metric;
            for (index, chunk) in wrap(&format!("{}{}: {}", if current { "▸ " } else { "  " }, metric.label(p), metric.describe(p)), inner.width as usize - 4).into_iter().enumerate() {
                lines.push(Line::from(Span::styled(if index == 0 { chunk } else { format!("    {chunk}") }, if current { text.add_modifier(Modifier::BOLD) } else { dim })));
            }
        }
        if let Some(work) = p.raw.work.as_ref().filter(|w| !w.fidelity.is_empty()) {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("The work clock beside a client", heading)));
            for chunk in wrap(work.fidelity.trim_start_matches("fidelity: "), inner.width as usize) {
                lines.push(Line::from(Span::styled(chunk, dim)));
            }
        }
    }
    Paragraph::new(lines).render(inner, buf);
}

/// Words wrapped to `width` columns
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// A key the global keys did not take
pub fn key(app: &mut App, key: Key) {
    match app.view {
        View::Overview => overview::key(app, key),
        View::Flame => flame::key(app, key),
        View::Calls => calls::key(app, key),
        View::Source => source::key(app, key),
        View::Alloc => alloc::key(app, key),
        View::Heap => heap::key(app, key),
        View::Timeline => timeline::key(app, key),
        View::Caches => caches::key(app, key),
        View::Issues => issues::key(app, key),
        View::Diff => diff::key(app, key),
        View::Compare => compare::key(app, key),
    }
}

/// What `y` copies in the current view
pub fn copy_text(app: &App) -> Option<String> {
    match app.view {
        View::Flame => flame::copy(app),
        View::Calls => calls::copy(app),
        View::Source => source::copy(app),
        View::Alloc => alloc::copy(app),
        View::Caches => caches::copy(app),
        View::Issues | View::Overview => issues::copy(app),
        View::Diff => diff::copy(app),
        View::Compare => compare::copy(app),
        View::Heap => heap::copy(app),
        View::Timeline => timeline::copy(app),
    }
}

/// `n`, `N` and a search's enter
pub fn find(app: &mut App, forward: bool, from_start: bool) -> bool {
    let query = app.query.to_lowercase();
    match app.view {
        View::Flame => flame::find(app, &query, forward, from_start),
        View::Calls => calls::find(app, &query),
        View::Source => source::find(app, &query, forward, from_start),
        View::Alloc => alloc::find(app, &query, forward, from_start),
        View::Caches => caches::find(app, &query, forward, from_start),
        View::Issues | View::Overview => issues::find(app, &query, forward, from_start),
        View::Diff => diff::find(app, &query, forward, from_start),
        _ => false,
    }
}

/// The next index after (or before) `from` in `0..len` whose item matches, wrapping round
pub fn next_match(len: usize, from: usize, forward: bool, from_start: bool, matches: impl Fn(usize) -> bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    for step in 0..len {
        let index = if from_start {
            if forward { step } else { len - 1 - step }
        } else if forward {
            (from + 1 + step) % len
        } else {
            (from + len - 1 - step % len) % len
        };
        if matches(index) {
            return Some(index);
        }
    }
    None
}

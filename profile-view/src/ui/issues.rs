//! Issues: the problems the measured patterns show, most severe first and costliest first within a severity, each with
//! the work it accounts for (ms a frame on the work clock), its numbers, what would remove it and a link (enter) to the
//! view and line that show it; what the profile could not check, and why, at the bottom.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use super::kit::{self, Item, col, col_fill, col_r};
use crate::{
    app::App,
    fmt,
    issues::{Issue, Severity, Target},
    keys::Key,
    theme::Theme,
};

/// A severity's colour, for its icon (the word beside it says the same)
pub fn severity_color(severity: Severity, theme: &Theme) -> ratatui::style::Color {
    match severity {
        Severity::Critical => theme.critical,
        Severity::Serious => theme.serious,
        Severity::Warning => theme.warning,
        Severity::Info => theme.muted,
    }
}

/// An issue's severity as a cell: its icon in its colour, its word
pub fn severity_cell(severity: Severity, theme: &Theme) -> Item {
    Item::Spans(Line::from(vec![
        Span::styled(format!("{} ", severity.icon()), Style::new().fg(severity_color(severity, theme))),
        Span::styled(severity.word().to_string(), Style::new().fg(theme.ink2)),
    ]))
}

/// Where enter goes, in words
pub fn target_text(app: &App, target: &Target) -> String {
    let p = &app.profiles[app.shown];
    match target {
        Target::None => String::new(),
        Target::Func(func) => format!("Calls: {}", p.funcs[*func as usize].label()),
        Target::Line(func, line) => format!("Source: {}:{line}", p.funcs[*func as usize].short),
        Target::Frame(row) => format!("Timeline: frame {}", p.raw.timeline.as_ref().and_then(|t| t.value(*row, "frame")).unwrap_or(*row as f64)),
        Target::Site(func, line) => format!("Allocations: {}:{line}", p.funcs[*func as usize].short),
        Target::Cache(_) => "Caches: the call site".into(),
        Target::Snapshot(index) => format!("Heap: snapshot {}", p.raw.heap.as_ref().and_then(|h| h.snapshots.get(*index)).map_or(String::new(), |s| s.label.clone())),
        Target::Variant(name) => format!("Compare: {name}"),
    }
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &mut App, theme: &Theme) {
    let issues = app.issues();
    let (found, unchecked) = (&issues.0, &issues.1);
    let budget = app.profiles.get(app.shown).map_or(1000.0 / 60.0, |p| p.budget_ms());
    let mut list = app.issues_list;
    // (every note, wrapped to the panel's text beside its mark: as many lines as they take, up to half the screen, the rest
    // counted)
    let note_width = (area.width.saturating_sub(6) as usize).max(10);
    let wrapped: Vec<Vec<String>> = unchecked.iter().map(|note| super::wrap(note, note_width)).collect();
    let needed = wrapped.iter().map(|lines| lines.len()).sum::<usize>() as u16;
    // (the list keeps room for its first few issues: the detail and the notes take what is left)
    let list_min = found.len().clamp(1, 4) as u16 + 3;
    let detail_height = 10u16.min(area.height / 3);
    let unchecked_height = if unchecked.is_empty() { 0 } else { (needed + 2).min(area.height.saturating_sub(list_min + detail_height).max(3)).min(area.height / 2) };
    let parts = kit::rows(area, &[area.height.saturating_sub(detail_height + unchecked_height), detail_height, unchecked_height]);
    // the issues
    let block = kit::panel(theme, "Issues", &fmt::grouped(found.len() as f64), true);
    let inner = kit::draw_panel(buf, parts[0], block);
    if found.is_empty() {
        kit::empty(buf, inner, theme, "none found");
    } else {
        list.fit(found.len(), kit::rows_shown(inner));
        let rows = found.iter().map(|issue| vec![severity_cell(issue.severity, theme), Item::text(issue.cost_short(budget), Style::new().fg(if issue.ms.is_some() || issue.once.is_some() { theme.ink } else { theme.muted })), Item::text(issue.kind, Style::new().fg(theme.muted)), Item::label(&issue.title, Style::new().fg(theme.ink))]).collect();
        let grid = kit::table(buf, inner, theme, &[col("severity", 10), col_r("cost", 13), col("kind", 18), col_fill("issue", 1)], rows, Some(list.selected), list.offset);
        list.offset = grid.offset;
    }
    app.issues_list = list;
    // the selected issue
    if let Some(issue) = found.get(list.selected) {
        draw_detail(buf, parts[1], app, issue, theme);
    }
    if unchecked_height > 0 {
        let inner = kit::draw_panel(buf, parts[2], kit::panel(theme, "Not checked", &format!("{} · what the issues do not cover", unchecked.len()), false));
        let room = inner.height as usize;
        let mut lines: Vec<Line> = Vec::new();
        // (the lines each note shown took)
        let mut shown: Vec<usize> = Vec::new();
        for note in &wrapped {
            if lines.len() + note.len() > room {
                break;
            }
            for (index, part) in note.iter().enumerate() {
                lines.push(Line::from(vec![Span::styled(if index == 0 { "○ " } else { "  " }, Style::new().fg(theme.muted)), Span::styled(part.clone(), Style::new().fg(theme.ink2))]));
            }
            shown.push(note.len());
        }
        // (what does not fit is counted, never dropped silently: whole notes make room for the count, never half of one)
        if shown.len() < wrapped.len() {
            while lines.len() + 1 > room {
                let Some(taken) = shown.pop() else { break };
                lines.truncate(lines.len() - taken);
            }
            lines.push(Line::from(Span::styled(format!("… and {} more (a taller screen shows them all)", wrapped.len() - shown.len()), Style::new().fg(theme.muted))));
        }
        Paragraph::new(lines).render(inner, buf);
    }
}

/// An issue in full: its numbers, what would remove it and where enter goes
pub fn draw_detail(buf: &mut Buffer, area: Rect, app: &App, issue: &Issue, theme: &Theme) {
    let color = severity_color(issue.severity, theme);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{} {}", issue.severity.icon(), issue.severity.word()), Style::new().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {} ", issue.title), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)),
    ]);
    let block = kit::panel_line(theme, title, false);
    let inner = kit::draw_panel(buf, area, block);
    let mut lines: Vec<Line> = issue.evidence.iter().map(|line| Line::from(vec![Span::styled("• ", Style::new().fg(theme.muted)), Span::styled(line.clone(), Style::new().fg(theme.ink))])).collect();
    if !issue.advice.is_empty() {
        lines.push(Line::from(vec![Span::styled("→ ", Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)), Span::styled(issue.advice.clone(), Style::new().fg(theme.ink2))]));
    }
    let target = target_text(app, &issue.target);
    if !target.is_empty() {
        lines.push(Line::from(vec![Span::styled("⏎ ", Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)), Span::styled(target, Style::new().fg(theme.muted))]));
    }
    Paragraph::new(lines).wrap(Wrap { trim: true }).render(inner, buf);
}

pub fn key(app: &mut App, key: Key) {
    let issues = app.issues();
    if app.issues_list.key(key, issues.0.len(), app.page) {
        return;
    }
    if key == Key::Enter {
        if let Some(issue) = issues.0.get(app.issues_list.selected) {
            let target = issue.target.clone();
            app.follow(&target);
        }
    }
}

pub fn copy(app: &App) -> Option<String> {
    let issues = app.issues();
    let list = if app.view == crate::app::View::Overview { app.overview } else { app.issues_list };
    let issue = issues.0.get(list.selected)?;
    let mut text = format!("{} {} ({}): {}", issue.severity.icon(), issue.severity.word(), issue.cost_text(), issue.title);
    for line in &issue.evidence {
        text.push_str(&format!("\n  {line}"));
    }
    Some(text)
}

pub fn find(app: &mut App, query: &str, forward: bool, from_start: bool) -> bool {
    let issues = app.issues();
    let from = if app.view == crate::app::View::Overview { app.overview.selected } else { app.issues_list.selected };
    match super::next_match(issues.0.len(), from, forward, from_start, |i| issues.0[i].title.to_lowercase().contains(query)) {
        Some(found) => {
            if app.view == crate::app::View::Overview {
                app.overview.selected = found;
            } else {
                app.issues_list.selected = found;
            }
            true
        }
        None => false,
    }
}

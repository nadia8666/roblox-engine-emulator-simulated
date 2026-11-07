//! The pieces the views are made of, on ratatui's widgets: a panel (a rounded border with its title and a note), a
//! table (its header raised, every other row shaded, the selected row washed and marked in the accent), bars,
//! sparklines and gauges for a table's cells, stat cards, the heat scale's legend and the keys' chips. One look for
//! every view, set here.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Cell, HighlightSpacing, LineGauge, Padding, Paragraph, Row, StatefulWidget, Table, TableState, Widget},
};

use crate::{
    fmt,
    theme::{HEAT_TICKS, Mark, Theme},
};

/// A panel: a rounded border with its title (bold; in the accent when it takes the keys) and a note after it
pub fn panel<'a>(theme: &Theme, title: &str, note: &str, focused: bool) -> Block<'a> {
    let title_style = Style::new().fg(if focused { theme.accent } else { theme.ink }).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" "), Span::styled(title.to_string(), title_style)];
    if !note.is_empty() {
        spans.push(Span::styled(format!("  {note}"), Style::new().fg(theme.muted)));
    }
    spans.push(Span::raw(" "));
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { theme.accent } else { theme.border }))
        .style(Style::new().bg(theme.panel).fg(theme.ink))
        .title(Line::from(spans))
        .padding(Padding::horizontal(1))
}

/// A panel whose title is a line of its own (spans in several styles)
pub fn panel_line<'a>(theme: &Theme, title: Line<'a>, focused: bool) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { theme.accent } else { theme.border }))
        .style(Style::new().bg(theme.panel).fg(theme.ink))
        .title(title)
        .padding(Padding::horizontal(1))
}

/// A note at a panel's top right (a scroll position, a scale's end)
pub fn note_right<'a>(block: Block<'a>, theme: &Theme, note: &str) -> Block<'a> {
    if note.is_empty() {
        return block;
    }
    block.title_top(Line::from(Span::styled(format!(" {note} "), Style::new().fg(theme.muted))).right_aligned())
}

/// A line along a panel's bottom border (a legend)
pub fn bottom<'a>(block: Block<'a>, line: Line<'a>) -> Block<'a> {
    block.title_bottom(line)
}

/// Draws a panel and returns its inside
pub fn draw_panel(buf: &mut Buffer, area: Rect, block: Block) -> Rect {
    let inner = block.inner(area);
    block.render(area, buf);
    inner
}

/// Text dimmed in the middle of an area (an empty panel's word)
pub fn empty(buf: &mut Buffer, area: Rect, theme: &Theme, text: &str) {
    if area.height == 0 {
        return;
    }
    let y = area.y + area.height.saturating_sub(1) / 2;
    Paragraph::new(Line::from(Span::styled(text.to_string(), Style::new().fg(theme.muted)))).alignment(Alignment::Center).render(Rect { y, height: 1, ..area }, buf);
}

/// A table's column: its title, its width and whether it lines up on the right
#[derive(Clone, Debug)]
pub struct Col {
    pub title: String,
    pub width: Constraint,
    pub right: bool,
}

pub fn col(title: &str, width: u16) -> Col {
    Col { title: title.into(), width: Constraint::Length(width), right: false }
}

pub fn col_r(title: &str, width: u16) -> Col {
    Col { title: title.into(), width: Constraint::Length(width), right: true }
}

/// A column that takes what the fixed ones leave (shared with the other fill columns by weight)
pub fn col_fill(title: &str, weight: u16) -> Col {
    Col { title: title.into(), width: Constraint::Fill(weight), right: false }
}

/// What a table's cell holds
#[derive(Clone, Debug)]
pub enum Item {
    /// A label `name  Module:line`: cut to the column keeping its line ([`fmt::fit_label`])
    Label(String, Style),
    /// Text cut to the column with an ellipsis
    Text(String, Style),
    /// Spans as they are (bars, sparklines, marks beside text)
    Spans(Line<'static>),
}

impl Item {
    pub fn label(text: impl Into<String>, style: Style) -> Item {
        Item::Label(text.into(), style)
    }

    pub fn text(text: impl Into<String>, style: Style) -> Item {
        Item::Text(text.into(), style)
    }

    pub fn empty() -> Item {
        Item::Text(String::new(), Style::new())
    }
}

/// The selection marker's width: the table's columns start after it
pub const MARKER: u16 = 2;

/// Where a table's columns and rows landed: each column's x and width, the first row's y and the rows shown
pub struct Grid {
    pub xs: Vec<u16>,
    pub widths: Vec<u16>,
    pub top: u16,
    pub offset: usize,
    pub shown: usize,
}

impl Grid {
    /// A column's cells on the rows shown
    pub fn column(&self, index: usize) -> Rect {
        Rect { x: self.xs[index], y: self.top, width: self.widths[index], height: self.shown as u16 }
    }
}

/// The columns' widths in an area as the table lays them out
pub fn widths(cols: &[Col], width: u16) -> Vec<u16> {
    let [_, columns] = Layout::horizontal([Constraint::Length(MARKER), Constraint::Fill(0)]).areas(Rect::new(0, 0, width, 1));
    Layout::horizontal(cols.iter().map(|c| c.width)).spacing(1).split(columns).iter().map(|r| r.width).collect()
}

/// The rows a table shows in an area (its header takes one)
pub fn rows_shown(area: Rect) -> usize {
    area.height.saturating_sub(1) as usize
}

/// A table: the header raised and bold, every other row shaded, the selected row (when there is one) washed, bold and
/// marked with the accent's bar. `offset` is the first row shown (the view's list keeps it)
pub fn table(buf: &mut Buffer, area: Rect, theme: &Theme, cols: &[Col], rows: Vec<Vec<Item>>, selected: Option<usize>, offset: usize) -> Grid {
    let widths = widths(cols, area.width);
    let header = Row::new(cols.iter().zip(&widths).map(|(c, w)| {
        let title = fmt::fit(&c.title, *w as usize);
        Cell::from(if c.right { Line::from(title).right_aligned() } else { Line::from(title) })
    }))
    .style(Style::new().bg(theme.raised).fg(theme.ink2).add_modifier(Modifier::BOLD));
    let shown = rows_shown(area).min(rows.len().saturating_sub(offset));
    let body: Vec<Row> = rows
        .into_iter()
        .enumerate()
        .map(|(index, items)| {
            let cells: Vec<Cell> = items
                .into_iter()
                .zip(cols.iter().zip(&widths))
                .map(|(item, (c, w))| {
                    let line = match item {
                        Item::Label(text, style) => Line::from(Span::styled(fmt::fit_label(&fmt::cells(&text), *w as usize), style)),
                        Item::Text(text, style) => Line::from(Span::styled(fmt::fit(&fmt::cells(&text), *w as usize), style)),
                        Item::Spans(line) => line,
                    };
                    Cell::from(if c.right { line.right_aligned() } else { line })
                })
                .collect();
            Row::new(cells).style(Style::new().bg(if index % 2 == 1 { theme.zebra } else { theme.panel }))
        })
        .collect();
    let table = Table::new(body, cols.iter().map(|c| c.width))
        .header(header)
        .column_spacing(1)
        .style(Style::new().bg(theme.panel).fg(theme.ink))
        .row_highlight_style(Style::new().bg(theme.wash).add_modifier(Modifier::BOLD))
        .highlight_symbol(Text::from(Span::styled("▌ ", Style::new().fg(theme.accent))))
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = TableState::default().with_offset(offset).with_selected(selected);
    StatefulWidget::render(table, area, buf, &mut state);
    let [_, columns] = Layout::horizontal([Constraint::Length(MARKER), Constraint::Fill(0)]).areas(area);
    let xs = Layout::horizontal(cols.iter().map(|c| c.width)).spacing(1).split(columns).iter().map(|r| r.x).collect();
    Grid { xs, widths, top: area.y + 1, offset: state.offset(), shown }
}

/// Marks an area's cells for the exporter (a bar column's cells drawn as bands)
pub fn mark(buf: &mut Buffer, area: Rect, mark: Mark) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.bg = Theme::marked(cell.bg, mark);
        }
    }
}

const EIGHTHS: [&str; 9] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█"];
const LEVELS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// A horizontal bar of `fraction` (0..1) of `width` columns, in eighths of a column (a value above zero shows at
/// least one)
pub fn bar(fraction: f64, width: u16, color: Color) -> Line<'static> {
    let fraction = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
    let eighths = (fraction * width as f64 * 8.0).round() as u32;
    let eighths = if fraction > 0.0 { eighths.max(1) } else { 0 };
    let mut text = String::new();
    for i in 0..width as u32 {
        let filled = eighths.saturating_sub(i * 8).min(8) as usize;
        if filled == 0 {
            break;
        }
        text.push_str(EIGHTHS[filled]);
    }
    Line::from(Span::styled(text, Style::new().fg(color)))
}

/// A two-sided bar for a change: blue to the left of a hairline for less, red to its right for more, `fraction` of
/// an arm (-1..1)
pub fn diverging(fraction: f64, width: u16, theme: &Theme) -> Line<'static> {
    let half = (width / 2) as usize;
    let fraction = if fraction.is_finite() { fraction.clamp(-1.0, 1.0) } else { 0.0 };
    let cells = ((fraction.abs() * half as f64).round() as usize).max(usize::from(fraction != 0.0)).min(half);
    let mut spans = Vec::new();
    if fraction < 0.0 {
        spans.push(Span::raw(" ".repeat(half - cells)));
        spans.push(Span::styled("█".repeat(cells), Style::new().fg(theme.less)));
    } else {
        spans.push(Span::raw(" ".repeat(half)));
    }
    spans.push(Span::styled("│", Style::new().fg(theme.faint)));
    if fraction > 0.0 {
        spans.push(Span::styled("█".repeat(cells), Style::new().fg(theme.more)));
    }
    Line::from(spans)
}

/// A one-row sparkline: one value a column, in eighths of the row, each column coloured by `color`
pub fn spark(values: &[f64], max: f64, color: impl Fn(usize) -> Color) -> Line<'static> {
    let spans: Vec<Span> = values
        .iter()
        .enumerate()
        .map(|(i, &value)| {
            // (at most seven eighths, so rows of sparklines stacked in a table stay apart; a value under a fourteenth
            // of the largest reads as none, as a row of slivers would hide the spikes)
            let level = if max > 0.0 { ((value / max) * 7.0).round().clamp(0.0, 7.0) as usize } else { 0 };
            Span::styled(LEVELS[level], Style::new().fg(color(i)))
        })
        .collect();
    Line::from(spans)
}

/// Values in at most `width` columns, each the largest of its frames (a spike stays visible)
pub fn bucket_max(values: &[f64], width: usize) -> Vec<f64> {
    if values.len() <= width || width == 0 {
        return values.to_vec();
    }
    let per = values.len() as f64 / width as f64;
    (0..width)
        .map(|i| {
            let from = (i as f64 * per) as usize;
            let to = (((i + 1) as f64 * per) as usize).max(from + 1).min(values.len());
            values[from..to].iter().copied().fold(0.0, f64::max)
        })
        .collect()
}

/// How a run's frames map to a chart's columns: many frames share a column (the worst of them drawn, so a spike
/// stays), few frames take several columns each with a gap between them
#[derive(Clone, Copy, Debug)]
pub struct FrameAxis {
    rows: usize,
    columns: usize,
    /// Columns a frame (its bar and a gap), 0 when frames share columns
    slot: usize,
    /// Frames a column, when they share
    per: f64,
}

impl FrameAxis {
    pub fn new(rows: usize, width: usize) -> FrameAxis {
        let slot = if rows > 0 && rows * 2 <= width { (width / rows).min(10) } else { 0 };
        let columns = if slot > 0 { rows * slot } else { rows.min(width) };
        let per = if slot > 0 { 1.0 } else { rows as f64 / columns.max(1) as f64 };
        FrameAxis { rows, columns, slot, per }
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Whether frames take several columns each (their bars already apart)
    pub fn stretched(&self) -> bool {
        self.slot > 0
    }

    /// The columns a frame's bar takes, [from, to)
    pub fn span(&self, row: usize) -> (usize, usize) {
        if self.slot > 0 {
            (row * self.slot, row * self.slot + self.slot - 1)
        } else {
            let column = ((row as f64 / self.per) as usize).min(self.columns.saturating_sub(1));
            (column, column + 1)
        }
    }

    /// The frame a column shows (its first, when frames share it), None for a gap
    pub fn row(&self, column: usize) -> Option<usize> {
        if self.slot > 0 {
            (column % self.slot != self.slot - 1).then_some(column / self.slot).filter(|row| *row < self.rows)
        } else {
            Some(((column as f64 * self.per) as usize).min(self.rows.saturating_sub(1)))
        }
    }

    /// A value a column
    pub fn values(&self, values: &[f64]) -> Vec<f64> {
        if self.slot > 0 {
            (0..self.columns).map(|column| self.row(column).map_or(0.0, |row| values.get(row).copied().unwrap_or(0.0))).collect()
        } else {
            bucket_max(values, self.columns)
        }
    }
}

/// A share as a thin gauge: its label, then a heavy line filled `ratio` of the way in `color`
pub fn gauge(buf: &mut Buffer, area: Rect, theme: &Theme, ratio: f64, label: Line<'static>, color: Color) {
    let ratio = if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.0 };
    LineGauge::default()
        .ratio(ratio)
        .label(label)
        .filled_symbol("━")
        .unfilled_symbol("━")
        .filled_style(Style::new().fg(color))
        .unfilled_style(Style::new().fg(theme.faint))
        .style(Style::new().bg(theme.panel).fg(theme.ink))
        .render(area, buf);
}

/// A stat card: a small panel with its figure large (bold) and a line under it
pub fn card(buf: &mut Buffer, area: Rect, theme: &Theme, title: &str, value: &str, under: Line<'static>) -> Rect {
    let inner = draw_panel(buf, area, panel(theme, title, "", false));
    if inner.height >= 1 {
        Paragraph::new(Line::from(Span::styled(value.to_string(), Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)))).render(Rect { height: 1, ..inner }, buf);
    }
    if inner.height >= 2 {
        Paragraph::new(under).render(Rect { y: inner.y + 1, height: 1, ..inner }, buf);
    }
    inner
}

/// The heat scale's legend: what it is a share of, grey for negligible, then the scale from 0.1% to 30% and more
/// with its marks under the shares
pub fn heat_legend(theme: &Theme, subject: &str, width: u16) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" share of {subject} "), Style::new().fg(theme.muted)),
        Span::styled("■", Style::new().fg(theme.negligible)),
        Span::styled(" <0.1% ", Style::new().fg(theme.muted)),
    ];
    // (the scale as a strip between the shares' words, each segment's cells blended along it)
    let cells = (width as usize / 4).clamp(3, 8);
    for (index, (share, word)) in HEAT_TICKS.iter().enumerate() {
        let last = index + 1 == HEAT_TICKS.len();
        spans.push(Span::styled(if last { format!(" {word}+ ") } else { format!(" {word} ") }, Style::new().fg(theme.ink2)));
        if let Some((next, _)) = HEAT_TICKS.get(index + 1) {
            let (from, to) = (Theme::heat_position(*share).unwrap_or(0.0), Theme::heat_position(*next).unwrap_or(1.0));
            for cell in 0..cells {
                let t = from + (to - from) * (cell as f64 + 0.5) / cells as f64;
                spans.push(Span::styled("█", Style::new().fg(theme.heat_at(t))));
            }
        }
    }
    Line::from(spans)
}

/// Keys and what they do, as chips: the key bold in the accent, its word muted
pub fn chips(theme: &Theme, keys: &[(&str, &str)], bg: Color) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, (key, word)) in keys.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", Style::new().bg(bg)));
        }
        spans.push(Span::styled(key.to_string(), Style::new().fg(theme.accent).bg(bg).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {word}"), Style::new().fg(theme.ink2).bg(bg)));
    }
    spans
}

/// The rows of an area split in the given heights (the last takes the rest)
pub fn rows(area: Rect, heights: &[u16]) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut y = area.y;
    for (index, &h) in heights.iter().enumerate() {
        let h = if index + 1 == heights.len() { area.bottom().saturating_sub(y) } else { h.min(area.bottom().saturating_sub(y)) };
        out.push(Rect { x: area.x, y, width: area.width, height: h });
        y += h;
    }
    out
}

/// An area split in columns `first` wide and the rest, a column of page between them
pub fn split(area: Rect, first: u16) -> (Rect, Rect) {
    let first = first.min(area.width);
    let left = Rect { width: first, ..area };
    let right = Rect { x: area.x + first + 1, width: area.width.saturating_sub(first + 1), ..area };
    (left, right)
}

//! Colour, one job each (the data-viz method ree's report page follows): a heat scale for how much of the run
//! something costs (in every view), a blue-red pair with a grey middle for less or more than before, categorical
//! slots in a fixed order for a comparison's variants and a run's phases, and the status colours, always with an
//! icon and a word, for the issues. Text stays in the ink colours; a coloured mark beside it carries the meaning.
//!
//! The heat scale runs through several hues, as inferno does (purple, crimson, orange, gold on the dark theme; the
//! same ramp turned round on the light one, so more is always further from the page): its lightness rises in even
//! steps of about 0.1 in OKLab, so its order reads in grey and under colour blindness, and each step's label ink is
//! the one APCA reads best on it. The categorical, diverging and status values are the method's reference palette,
//! checked against these surfaces (8 slots, every adjacent pair 8.4 apart or more under protanopia and deuteranopia).
//!
//! Chrome: a page behind rounded panels, a raised colour for the bars and a table's header, every other table row a
//! shade off the panel, and one accent (the active tab, the panel that takes the keys, the keys in the footer).

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    pub fn parse(name: &str) -> Option<ThemeKind> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(ThemeKind::Dark),
            "light" => Some(ThemeKind::Light),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ThemeKind::Dark => "dark",
            ThemeKind::Light => "light",
        }
    }
}

/// The share of the run's total the heat scale starts at (below it a figure is negligible and drawn grey) and ends at
pub const HEAT_FROM: f64 = 0.001;
pub const HEAT_TO: f64 = 0.30;
/// The shares the legend marks on the scale
pub const HEAT_TICKS: [(f64, &str); 5] = [(0.001, "0.1%"), (0.01, "1%"), (0.03, "3%"), (0.10, "10%"), (0.30, "30%")];

#[derive(Clone, Debug)]
pub struct Theme {
    pub kind: ThemeKind,
    /// Behind everything, and inside the panels (one colour: a panel is its rounded border)
    pub page: Color,
    pub panel: Color,
    /// The header and footer bars, a table's header row, a key's chip
    pub raised: Color,
    /// Every other row of a table
    pub zebra: Color,
    /// A selected row's background
    pub wash: Color,
    /// A panel's border
    pub border: Color,
    /// The active tab, the border of the panel that takes the keys, the keys in the footer
    pub accent: Color,
    /// Text on the accent
    pub accent_ink: Color,
    pub ink: Color,
    pub ink2: Color,
    pub muted: Color,
    /// Tracks, axes and empty bar cells
    pub faint: Color,
    /// The heat scale's stops, from 0.1% of the run to 30% and more
    pub heat_stops: [Color; 6],
    /// A node under 0.1% of the run in the flame graph, and a phase's node
    pub negligible: Color,
    pub phase_node: Color,
    /// Ink on a heat colour: the light and the dark
    pub ink_on_dark: Color,
    pub ink_on_light: Color,
    /// More than before (worse for a cost), and less
    pub more: Color,
    pub less: Color,
    pub series: [Color; 8],
    pub good: Color,
    pub warning: Color,
    pub serious: Color,
    pub critical: Color,
    /// A search match's background, and its ink
    pub found: Color,
    pub found_ink: Color,
}

/// What a cell is drawn as by the exporter, carried in the two low bits of its background's blue (a step the eye
/// cannot see): a flame graph's node (a gap above its row), a horizontal bar's cell (drawn as a band, a gap between
/// rows) or a column chart's (a gap between columns)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    None,
    Node,
    Bar,
    Column,
}

/// A colour from its hex, its blue's two low bits clear (they carry a cell's mark)
pub const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8 & 0xfc)
}

impl Theme {
    pub fn new(kind: ThemeKind) -> Theme {
        match kind {
            ThemeKind::Dark => Theme {
                kind,
                page: rgb(0x13151b),
                panel: rgb(0x13151b),
                raised: rgb(0x1f232d),
                zebra: rgb(0x181b22),
                wash: rgb(0x223152),
                border: rgb(0x474f66),
                accent: rgb(0x6fcbe8),
                accent_ink: rgb(0x13151b),
                ink: rgb(0xe9ebf1),
                ink2: rgb(0xaab1c1),
                muted: rgb(0x727a8c),
                faint: rgb(0x343a4a),
                heat_stops: [rgb(0x6b2079), rgb(0x962a68), rgb(0xc83f4e), rgb(0xea642c), rgb(0xf99c17), rgb(0xf8d84a)],
                negligible: rgb(0x2a3040),
                phase_node: rgb(0x323a4d),
                ink_on_dark: rgb(0xf1f2f6),
                ink_on_light: rgb(0x14151a),
                more: rgb(0xe66767),
                less: rgb(0x6da7ec),
                series: [rgb(0x3987e5), rgb(0xd95926), rgb(0x199e70), rgb(0xc98500), rgb(0xd55181), rgb(0x008300), rgb(0x9085e9), rgb(0xe66767)],
                good: rgb(0x0ca30c),
                warning: rgb(0xfab219),
                serious: rgb(0xec835a),
                critical: rgb(0xd03b3b),
                found: rgb(0x9085e9),
                found_ink: rgb(0x13151b),
            },
            ThemeKind::Light => Theme {
                kind,
                page: rgb(0xfbfbfc),
                panel: rgb(0xfbfbfc),
                raised: rgb(0xe8ebf0),
                zebra: rgb(0xf3f4f7),
                wash: rgb(0xd7e6fb),
                border: rgb(0xc3c9d5),
                accent: rgb(0x0b72a8),
                accent_ink: rgb(0xffffff),
                ink: rgb(0x161a22),
                ink2: rgb(0x465063),
                muted: rgb(0x7a8295),
                faint: rgb(0xdde1e8),
                heat_stops: [rgb(0xf9d77e), rgb(0xf6a63c), rgb(0xe96a2e), rgb(0xc93e4c), rgb(0x8f2767), rgb(0x561a6f)],
                negligible: rgb(0xe8eaef),
                phase_node: rgb(0xd5dae4),
                ink_on_dark: rgb(0xffffff),
                ink_on_light: rgb(0x16171c),
                more: rgb(0xe34948),
                less: rgb(0x256abf),
                series: [rgb(0x2a78d6), rgb(0xeb6834), rgb(0x1baf7a), rgb(0xeda100), rgb(0xe87ba4), rgb(0x008300), rgb(0x6250d6), rgb(0xe34948)],
                good: rgb(0x0ca30c),
                warning: rgb(0xfab219),
                serious: rgb(0xec835a),
                critical: rgb(0xd03b3b),
                found: rgb(0x6250d6),
                found_ink: rgb(0xffffff),
            },
        }
    }

    /// A colour with a mark for the exporter (Mark::None clears it)
    pub fn marked(color: Color, mark: Mark) -> Color {
        let Color::Rgb(r, g, b) = color else { return color };
        let bits = match mark {
            Mark::None => 0,
            Mark::Node => 1,
            Mark::Bar => 2,
            Mark::Column => 3,
        };
        Color::Rgb(r, g, (b & 0xfc) | bits)
    }

    /// A cell's background as it looks, and its mark
    pub fn unmark(color: Color) -> (Color, Mark) {
        let Color::Rgb(r, g, b) = color else { return (color, Mark::None) };
        let mark = match b & 3 {
            1 => Mark::Node,
            2 => Mark::Bar,
            3 => Mark::Column,
            _ => Mark::None,
        };
        (Color::Rgb(r, g, b & 0xfc), mark)
    }

    /// Where a share of the run's total sits on the heat scale (0 at 0.1%, 1 at 30% and more), None when negligible
    pub fn heat_position(share: f64) -> Option<f64> {
        if !(share >= HEAT_FROM) {
            return None;
        }
        Some(((share / HEAT_FROM).log10() / (HEAT_TO / HEAT_FROM).log10()).clamp(0.0, 1.0))
    }

    /// The heat colour at a position on the scale, blended between its stops in OKLab
    pub fn heat_at(&self, t: f64) -> Color {
        let t = t.clamp(0.0, 1.0) * (self.heat_stops.len() - 1) as f64;
        let at = (t.floor() as usize).min(self.heat_stops.len() - 2);
        mix(self.heat_stops[at], self.heat_stops[at + 1], t - at as f64)
    }

    /// The colour of a share of the run's total (muted when negligible)
    pub fn heat_of(&self, share: f64) -> Color {
        Theme::heat_position(share).map_or(self.muted, |t| self.heat_at(t))
    }

    /// A flame node's colour: its heat, or grey when negligible
    pub fn node_of(&self, share: f64) -> Color {
        Theme::heat_position(share).map_or(self.negligible, |t| self.heat_at(t))
    }

    /// The ink APCA reads best on a fill
    pub fn ink_on(&self, fill: Color) -> Color {
        if lightness(fill) > 0.72 { self.ink_on_light } else { self.ink_on_dark }
    }

    /// A categorical slot, in its fixed order (a ninth folds back: comparisons and phases stay under eight)
    pub fn slot(&self, index: usize) -> Color {
        self.series[index % self.series.len()]
    }

    pub fn text(&self) -> Style {
        Style::new().fg(self.ink).bg(self.panel)
    }

    pub fn secondary(&self) -> Style {
        Style::new().fg(self.ink2).bg(self.panel)
    }

    pub fn dim(&self) -> Style {
        Style::new().fg(self.muted).bg(self.panel)
    }

    pub fn bold(&self) -> Style {
        self.text().add_modifier(Modifier::BOLD)
    }

    /// Text in a colour on the panel (a mark's: an icon, a swatch)
    pub fn mark(&self, color: Color) -> Style {
        Style::new().fg(color).bg(self.panel)
    }

    /// Ink for a row: negligible rows recede
    pub fn row(&self, share: f64) -> Style {
        if Theme::heat_position(share).is_some() { self.text() } else { self.dim() }
    }

    /// Foreground-only styles, for spans laid over a row's own background (zebra, selection)
    pub fn fg(&self, color: Color) -> Style {
        Style::new().fg(color)
    }
}

/// OKLab lightness of a colour (0 black, 1 white)
pub fn lightness(color: Color) -> f64 {
    let Color::Rgb(r, g, b) = color else { return 0.5 };
    oklab(r, g, b).0
}

fn linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn gamma(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

fn oklab(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    (
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )
}

fn from_oklab(l: f64, a: f64, b: f64) -> (u8, u8, u8) {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    (
        gamma(4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_),
        gamma(-1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_),
        gamma(-0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_),
    )
}

/// Two colours blended in OKLab, `t` of the way from `a` to `b` (its blue's low bits clear)
pub fn mix(a: Color, b: Color, t: f64) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else { return if t < 0.5 { a } else { b } };
    let (l1, a1, b1) = oklab(ar, ag, ab);
    let (l2, a2, b2) = oklab(br, bg, bb);
    let (r, g, b) = from_oklab(l1 + (l2 - l1) * t, a1 + (a2 - a1) * t, b1 + (b2 - b1) * t);
    Color::Rgb(r, g, b & 0xfc)
}

/// Whether the terminal shows 24-bit colour: `REE_VIEW_COLOR=truecolor|256` decides, else COLORTERM, Windows
/// Terminal and the terminals known to (Windows' own console has since Windows 10)
pub fn truecolor() -> bool {
    match std::env::var("REE_VIEW_COLOR").map(|v| v.to_ascii_lowercase()) {
        Ok(v) if v == "256" => return false,
        Ok(v) if v == "truecolor" || v == "24bit" => return true,
        _ => {}
    }
    if std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit")) {
        return true;
    }
    if std::env::var_os("WT_SESSION").is_some() || cfg!(windows) {
        return true;
    }
    std::env::var("TERM_PROGRAM").is_ok_and(|v| matches!(v.as_str(), "vscode" | "WezTerm" | "iTerm.app" | "ghostty" | "rio" | "Hyper"))
}

/// The nearest of the 256 colours a terminal without 24-bit colour has (its 6x6x6 cube and its greys), by distance
/// in OKLab
pub fn to_256(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else { return color };
    let target = oklab(r, g, b);
    let distance = |c: (u8, u8, u8)| {
        let o = oklab(c.0, c.1, c.2);
        (o.0 - target.0).powi(2) + (o.1 - target.1).powi(2) + (o.2 - target.2).powi(2)
    };
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| LEVELS.iter().enumerate().min_by_key(|(_, l)| (**l as i32 - v as i32).abs()).map_or(0, |(i, _)| i);
    // (the cube's nearest corner by channel, and its neighbours one step round it, and every grey)
    let (cr, cg, cb) = (near(r), near(g), near(b));
    let mut best = (16 + 36 * cr + 6 * cg + cb, distance((LEVELS[cr], LEVELS[cg], LEVELS[cb])));
    for dr in -1i32..=1 {
        for dg in -1i32..=1 {
            for db in -1i32..=1 {
                let (ir, ig, ib) = (cr as i32 + dr, cg as i32 + dg, cb as i32 + db);
                if !(0..6).contains(&ir) || !(0..6).contains(&ig) || !(0..6).contains(&ib) {
                    continue;
                }
                let d = distance((LEVELS[ir as usize], LEVELS[ig as usize], LEVELS[ib as usize]));
                if d < best.1 {
                    best = ((16 + 36 * ir + 6 * ig + ib) as usize, d);
                }
            }
        }
    }
    for step in 0..24u8 {
        let v = 8 + 10 * step;
        let d = distance((v, v, v));
        if d < best.1 {
            best = (232 + step as usize, d);
        }
    }
    Color::Indexed(best.0 as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_heat_scale_rises_in_lightness_on_dark_and_falls_on_light() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = Theme::new(kind);
            let steps: Vec<f64> = (0..=20).map(|i| lightness(theme.heat_at(i as f64 / 20.0))).collect();
            for pair in steps.windows(2) {
                match kind {
                    ThemeKind::Dark => assert!(pair[1] > pair[0], "{kind:?}: {steps:?}"),
                    ThemeKind::Light => assert!(pair[1] < pair[0], "{kind:?}: {steps:?}"),
                }
            }
        }
        assert_eq!(Theme::heat_position(0.0005), None);
        assert_eq!(Theme::heat_position(0.001), Some(0.0));
        assert_eq!(Theme::heat_position(0.5), Some(1.0));
    }

    #[test]
    fn marks_ride_on_the_blue_channel_and_come_off_again() {
        let theme = Theme::new(ThemeKind::Dark);
        for mark in [Mark::None, Mark::Node, Mark::Bar, Mark::Column] {
            assert_eq!(Theme::unmark(Theme::marked(theme.panel, mark)), (theme.panel, mark));
        }
        // (every colour the theme names is unmarked)
        assert_eq!(Theme::unmark(theme.heat_at(0.37)).1, Mark::None);
    }

    #[test]
    fn a_terminal_without_24_bit_colour_gets_the_nearest_of_its_256() {
        assert_eq!(to_256(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(to_256(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        assert_eq!(to_256(Color::Rgb(128, 128, 128)), Color::Indexed(244));
        assert_eq!(to_256(Color::Indexed(3)), Color::Indexed(3));
    }
}

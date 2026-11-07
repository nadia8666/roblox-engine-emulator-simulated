//! A drawn screen (the cell buffer the terminal shows) as a file: plain text for agents to read, SVG (text with each
//! character placed on the grid) and PNG (the cells rasterized with the bundled JetBrains Mono, fonts/; block elements,
//! box lines and braille are drawn as shapes so bars and plots join up exactly). The same buffer gives the same bytes on
//! every machine.

use std::{collections::HashMap, fmt::Write as _, path::Path};

use fontdue::{Font, FontSettings, Metrics};
use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier},
};

use crate::theme::{Mark, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Svg,
    Text,
}

impl Format {
    pub fn of_path(path: &Path) -> Option<Format> {
        match path.extension()?.to_string_lossy().to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "svg" => Some(Format::Svg),
            "txt" | "text" => Some(Format::Text),
            _ => None,
        }
    }

    pub fn parse(name: &str) -> Option<Format> {
        match name.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Format::Png),
            "svg" => Some(Format::Svg),
            "txt" | "text" => Some(Format::Text),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Svg => "svg",
            Format::Text => "txt",
        }
    }
}

/// A cell's size in pixels at scale 1, and the text's size
pub const CELL_W: u32 = 9;
pub const CELL_H: u32 = 20;
pub const FONT_PX: u32 = 15;

/// A cell's symbol as one character (every symbol the views draw is one)
fn symbol(buf: &Buffer, x: u16, y: u16) -> char {
    buf[(x, y)].symbol().chars().next().unwrap_or(' ')
}

/// The screen as text: a line a row, trailing spaces left out
pub fn text(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::with_capacity((area.width as usize + 1) * area.height as usize);
    for y in area.top()..area.bottom() {
        let mut line = String::with_capacity(area.width as usize);
        for x in area.left()..area.right() {
            line.push(symbol(buf, x, y));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn rgb(color: Color, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Gray => (192, 192, 192),
        Color::DarkGray => (128, 128, 128),
        Color::Red | Color::LightRed => (205, 49, 49),
        Color::Green | Color::LightGreen => (13, 188, 121),
        Color::Yellow | Color::LightYellow => (229, 229, 16),
        Color::Blue | Color::LightBlue => (36, 114, 200),
        Color::Magenta | Color::LightMagenta => (188, 63, 188),
        Color::Cyan | Color::LightCyan => (17, 168, 205),
        Color::Indexed(_) | Color::Reset => default,
    }
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// How a cell is drawn
#[derive(Clone, Copy, PartialEq, Eq)]
struct Look {
    bg: (u8, u8, u8),
    fg: (u8, u8, u8),
    bold: bool,
    underline: bool,
    /// A bar's, a column chart's or a flame graph node's cell (Theme::unmark)
    mark: Mark,
}

fn look(buf: &Buffer, x: u16, y: u16, theme: &Theme) -> Look {
    let cell = &buf[(x, y)];
    let page = rgb(theme.page, (19, 21, 27));
    let ink = rgb(theme.ink, (233, 235, 241));
    let (bg, mark) = Theme::unmark(cell.bg);
    let (fg, _) = Theme::unmark(cell.fg);
    let (mut bg, mut fg) = (rgb(bg, page), rgb(fg, ink));
    if cell.modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut bg, &mut fg);
    }
    Look { bg, fg, bold: cell.modifier.contains(Modifier::BOLD), underline: cell.modifier.contains(Modifier::UNDERLINED), mark }
}

/// A block's filled part in pixels, (left, top, right, bottom) within a cell of `w` x `h`: a bar's is a band through
/// the middle half of the cell (thin marks, a gap between rows), a column's leaves a gap at its right
fn block_px(ch: char, mark: Mark, w: f32, h: f32, scale: f32) -> Option<(f32, f32, f32, f32)> {
    let (l, t, r, b) = block(ch)?;
    let (x0, mut y0, mut x1, mut y1) = (l as f32 * w / 8.0, t as f32 * h / 8.0, r as f32 * w / 8.0, b as f32 * h / 8.0);
    match mark {
        Mark::Bar => {
            let inset = (h / 4.0).round();
            y0 = y0.max(inset);
            y1 = y1.min(h - inset);
        }
        Mark::Column => x1 = x1.min(w - scale),
        Mark::None | Mark::Node => {}
    }
    if x1 <= x0 || y1 <= y0 {
        return Some((0.0, 0.0, 0.0, 0.0));
    }
    Some((x0, y0, x1, y1))
}

/// A shape drawn instead of a glyph: (left, top, right, bottom) of the cell's filled part, in eighths
fn block(ch: char) -> Option<(u8, u8, u8, u8)> {
    let code = ch as u32;
    Some(match code {
        0x2588 => (0, 0, 8, 8),
        // ▉ ▊ ▋ ▌ ▍ ▎ ▏: the left 7/8 .. 1/8
        0x2589..=0x258f => (0, 0, (0x2590 - code) as u8, 8),
        // ▁ .. ▇: the lower 1/8 .. 7/8
        0x2581..=0x2587 => (0, 8 - (code - 0x2580) as u8, 8, 8),
        0x2580 => (0, 0, 8, 4),
        0x2590 => (4, 0, 8, 8),
        0x2594 => (0, 0, 8, 1),
        0x2595 => (7, 0, 8, 8),
        _ => return None,
    })
}

/// The SVG of a screen: a rectangle a run of background, a text element a run of text (each character placed on the
/// grid, so any monospace font lines up), block elements as rectangles
pub fn svg(buf: &Buffer, theme: &Theme, scale: u32) -> String {
    let (cell_w, cell_h, font_px, baseline) = (CELL_W * scale, CELL_H * scale, FONT_PX * scale, 15 * scale);
    let area = buf.area;
    let (width, height) = (area.width as u32 * cell_w, area.height as u32 * cell_h);
    let mut out = String::new();
    let _ = writeln!(out, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="'JetBrains Mono', 'Cascadia Mono', Consolas, 'DejaVu Sans Mono', monospace" font-size="{font_px}">"#);
    let _ = writeln!(out, r#"<rect width="{width}" height="{height}" fill="{}"/>"#, hex(rgb(theme.page, (19, 21, 27))));
    let surface = rgb(theme.page, (19, 21, 27));
    let gap = scale;
    for y in area.top()..area.bottom() {
        // backgrounds (a flame node's leaves a gap above it)
        let mut x = area.left();
        while x < area.right() {
            let first = look(buf, x, y, theme);
            let start = x;
            while x < area.right() {
                let next = look(buf, x, y, theme);
                if next.bg != first.bg || (next.mark == Mark::Node) != (first.mark == Mark::Node) {
                    break;
                }
                x += 1;
            }
            if first.bg != surface {
                let top = if first.mark == Mark::Node { gap } else { 0 };
                let _ = writeln!(out, r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#, start as u32 * cell_w, y as u32 * cell_h + top, (x - start) as u32 * cell_w, cell_h - top, hex(first.bg));
            }
        }
        // blocks, then text runs of one colour and weight
        let mut x = area.left();
        while x < area.right() {
            let ch = symbol(buf, x, y);
            let first = look(buf, x, y, theme);
            if let Some((l, t, r, b)) = block_px(ch, first.mark, cell_w as f32, cell_h as f32, scale as f32) {
                let (x0, y0) = (x as u32 * cell_w, y as u32 * cell_h);
                if r > l {
                    let _ = writeln!(out, r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{}"/>"#, x0 as f32 + l, y0 as f32 + t, r - l, b - t, hex(first.fg));
                }
                x += 1;
                continue;
            }
            if ch == ' ' && !first.underline {
                x += 1;
                continue;
            }
            let start = x;
            let mut run = String::new();
            let mut xs: Vec<String> = Vec::new();
            while x < area.right() {
                let c = symbol(buf, x, y);
                let next = look(buf, x, y, theme);
                if (next.fg, next.bold, next.underline) != (first.fg, first.bold, first.underline) || block(c).is_some() || (c == ' ' && !first.underline && x > start) {
                    break;
                }
                match c {
                    '&' => run.push_str("&amp;"),
                    '<' => run.push_str("&lt;"),
                    '>' => run.push_str("&gt;"),
                    c => run.push(c),
                }
                xs.push((x as u32 * cell_w).to_string());
                x += 1;
            }
            let weight = if first.bold { r#" font-weight="bold""# } else { "" };
            let decoration = if first.underline { r#" text-decoration="underline""# } else { "" };
            let _ = writeln!(out, r#"<text x="{}" y="{}" fill="{}"{weight}{decoration} xml:space="preserve">{run}</text>"#, xs.join(" "), y as u32 * cell_h + baseline, hex(first.fg));
        }
    }
    out.push_str("</svg>\n");
    out
}

/// The bundled font (fonts/: JetBrains Mono 2.304, SIL Open Font License 1.1, fonts/OFL.txt)
static REGULAR: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");
static BOLD: &[u8] = include_bytes!("../fonts/JetBrainsMono-Bold.ttf");

/// The font at one size, its glyphs rasterized once each
pub struct CellFont {
    regular: Font,
    bold: Font,
    px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    baseline: f32,
    glyphs: HashMap<(char, bool), (Metrics, Vec<u8>)>,
}

impl CellFont {
    /// The font for a cell grid at `scale` (1: 9x20 pixel cells, 15 px text)
    pub fn new(scale: u32) -> CellFont {
        let settings = FontSettings { scale: (FONT_PX * scale) as f32, ..FontSettings::default() };
        let regular = Font::from_bytes(REGULAR, settings).expect("the bundled font");
        let bold = Font::from_bytes(BOLD, settings).expect("the bundled bold font");
        let px = (FONT_PX * scale) as f32;
        let line = regular.horizontal_line_metrics(px).expect("horizontal metrics");
        // (the grid SVG uses too: JetBrains Mono's advance is 0.6 em, 9 px at 15 px)
        let cell_w = CELL_W * scale;
        let cell_h = CELL_H * scale;
        // (the text's ascent and descent centred in the cell)
        let content = line.ascent - line.descent;
        let baseline = ((cell_h as f32 - content) / 2.0 + line.ascent).round();
        CellFont { regular, bold, px, cell_w, cell_h, baseline, glyphs: HashMap::new() }
    }

    /// Whether the font has a glyph for the character (a shape drawn in its place counts)
    pub fn covers(&self, ch: char) -> bool {
        ch == ' ' || block(ch).is_some() || shape(ch) || self.regular.lookup_glyph_index(ch) != 0
    }

    fn glyph(&mut self, ch: char, bold: bool) -> &(Metrics, Vec<u8>) {
        let (font, px) = (if bold { &self.bold } else { &self.regular }, self.px);
        self.glyphs.entry((ch, bold)).or_insert_with(|| font.rasterize(ch, px))
    }
}

/// Characters drawn as lines, arcs and dots rather than glyphs: box lines (a panel's rounded corners among them) and
/// braille, so borders and plots join up exactly at any cell size
fn shape(ch: char) -> bool {
    matches!(ch, '─' | '━' | '│' | '┃' | '┬' | '┴' | '├' | '┤' | '┼' | '└' | '┘' | '┌' | '┐' | '╭' | '╮' | '╰' | '╯') || ('\u{2800}'..='\u{28ff}').contains(&ch)
}

/// An RGB image
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    fn new(width: u32, height: u32, color: (u8, u8, u8)) -> Image {
        let mut pixels = Vec::with_capacity((width * height * 3) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[color.0, color.1, color.2]);
        }
        Image { width, height, pixels }
    }

    /// Blends `color` over the pixel by `alpha` (0..1)
    fn blend(&mut self, x: i64, y: i64, color: (u8, u8, u8), alpha: f32) {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 || alpha <= 0.0 {
            return;
        }
        let alpha = alpha.min(1.0);
        let at = ((y as u32 * self.width + x as u32) * 3) as usize;
        for (channel, value) in [color.0, color.1, color.2].into_iter().enumerate() {
            let old = self.pixels[at + channel] as f32;
            self.pixels[at + channel] = (old + (value as f32 - old) * alpha).round() as u8;
        }
    }

    /// A rectangle with fractional edges, the edge pixels blended by how much of them it covers
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: (u8, u8, u8)) {
        for py in y0.floor() as i64..y1.ceil() as i64 {
            let cover_y = (y1.min(py as f32 + 1.0) - y0.max(py as f32)).clamp(0.0, 1.0);
            for px in x0.floor() as i64..x1.ceil() as i64 {
                let cover_x = (x1.min(px as f32 + 1.0) - x0.max(px as f32)).clamp(0.0, 1.0);
                self.blend(px, py, color, cover_x * cover_y);
            }
        }
    }

    /// A filled circle, its edge antialiased
    fn dot(&mut self, cx: f32, cy: f32, r: f32, color: (u8, u8, u8)) {
        for py in (cy - r - 1.0).floor() as i64..=(cy + r + 1.0).ceil() as i64 {
            for px in (cx - r - 1.0).floor() as i64..=(cx + r + 1.0).ceil() as i64 {
                let d = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
                self.blend(px, py, color, (r + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }
}

/// Draws a box line or braille character in a cell
fn draw_shape(image: &mut Image, ch: char, x0: f32, y0: f32, w: f32, h: f32, scale: f32, color: (u8, u8, u8)) {
    let (mx, my) = ((x0 + w / 2.0).floor(), (y0 + h / 2.0).floor());
    let thin = scale.max(1.0);
    let heavy = (2.0 * scale).max(2.0);
    let horizontal = |image: &mut Image, from: f32, to: f32, t: f32| image.rect(from, my - (t / 2.0).floor(), to, my - (t / 2.0).floor() + t, color);
    let vertical = |image: &mut Image, from: f32, to: f32, t: f32| image.rect(mx - (t / 2.0).floor(), from, mx - (t / 2.0).floor() + t, to, color);
    match ch {
        '─' => horizontal(image, x0, x0 + w, thin),
        '━' => horizontal(image, x0, x0 + w, heavy),
        '│' => vertical(image, y0, y0 + h, thin),
        '┃' => vertical(image, y0, y0 + h, heavy),
        '┬' => {
            horizontal(image, x0, x0 + w, thin);
            vertical(image, my, y0 + h, thin);
        }
        '┴' => {
            horizontal(image, x0, x0 + w, thin);
            vertical(image, y0, my + thin, thin);
        }
        '├' => {
            vertical(image, y0, y0 + h, thin);
            horizontal(image, mx, x0 + w, thin);
        }
        '┤' => {
            vertical(image, y0, y0 + h, thin);
            horizontal(image, x0, mx + thin, thin);
        }
        '┼' => {
            vertical(image, y0, y0 + h, thin);
            horizontal(image, x0, x0 + w, thin);
        }
        '└' => {
            vertical(image, y0, my + thin, thin);
            horizontal(image, mx, x0 + w, thin);
        }
        '┘' => {
            vertical(image, y0, my + thin, thin);
            horizontal(image, x0, mx + thin, thin);
        }
        '┌' => {
            vertical(image, my, y0 + h, thin);
            horizontal(image, mx, x0 + w, thin);
        }
        '┐' => {
            vertical(image, my, y0 + h, thin);
            horizontal(image, x0, mx + thin, thin);
        }
        '╭' | '╮' | '╰' | '╯' => {
            // a quarter circle between the two lines it joins, on their centres
            let (sx, sy) = match ch {
                '╭' => (1.0, 1.0),
                '╮' => (-1.0, 1.0),
                '╰' => (1.0, -1.0),
                _ => (-1.0, -1.0),
            };
            let r = (w * 0.45).round().max(2.0);
            let (lx, ly) = (mx - (thin / 2.0).floor() + thin / 2.0, my - (thin / 2.0).floor() + thin / 2.0);
            let (cx, cy) = (lx + sx * r, ly + sy * r);
            if sx > 0.0 {
                horizontal(image, cx, x0 + w, thin);
            } else {
                horizontal(image, x0, cx, thin);
            }
            if sy > 0.0 {
                vertical(image, cy, y0 + h, thin);
            } else {
                vertical(image, y0, cy, thin);
            }
            for py in (cy - r - 2.0).floor() as i64..=(cy + r + 2.0).ceil() as i64 {
                for px in (cx - r - 2.0).floor() as i64..=(cx + r + 2.0).ceil() as i64 {
                    let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                    if (fx - cx) * sx > 0.5 || (fy - cy) * sy > 0.5 {
                        continue;
                    }
                    let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                    image.blend(px, py, color, (thin / 2.0 + 0.5 - (d - r).abs()).clamp(0.0, 1.0));
                }
            }
        }
        _ => {
            // braille: dots 1-3 and 7 down the left column, 4-6 and 8 down the right
            let bits = ch as u32 - 0x2800;
            const DOTS: [(u32, u32, u32); 8] = [(0x01, 0, 0), (0x02, 0, 1), (0x04, 0, 2), (0x40, 0, 3), (0x08, 1, 0), (0x10, 1, 1), (0x20, 1, 2), (0x80, 1, 3)];
            let r = (w * 0.21).max(1.0);
            for (bit, col, row) in DOTS {
                if bits & bit != 0 {
                    image.dot(x0 + w * (0.27 + 0.46 * col as f32), y0 + h * (0.125 + 0.25 * row as f32), r, color);
                }
            }
        }
    }
}

/// The screen rasterized: a cell `font.cell_w` x `font.cell_h` pixels
pub fn raster(buf: &Buffer, theme: &Theme, font: &mut CellFont, scale: u32) -> Image {
    let area = buf.area;
    let (cw, ch) = (font.cell_w, font.cell_h);
    let mut image = Image::new(area.width as u32 * cw, area.height as u32 * ch, rgb(theme.page, (19, 21, 27)));
    let gap = scale as f32;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let Look { bg, fg, bold, underline, mark } = look(buf, x, y, theme);
            let node = mark == Mark::Node;
            let (x0, y0) = ((x - area.x) as f32 * cw as f32, (y - area.y) as f32 * ch as f32);
            image.rect(x0, y0 + if node { gap } else { 0.0 }, x0 + cw as f32, y0 + ch as f32, bg);
            let c = symbol(buf, x, y);
            if let Some((l, t, r, b)) = block_px(c, mark, cw as f32, ch as f32, scale as f32) {
                if r > l {
                    image.rect(x0 + l, y0 + t, x0 + r, y0 + b, fg);
                }
            } else if shape(c) {
                draw_shape(&mut image, c, x0, y0, cw as f32, ch as f32, scale as f32, fg);
            } else if c != ' ' {
                let baseline = font.baseline;
                let cell_w = cw as f32;
                let (metrics, bitmap) = font.glyph(c, bold);
                if metrics.width == 0 {
                    // (a character the font lacks: an outlined box, so it shows rather than vanishing)
                    if !c.is_whitespace() {
                        image.rect(x0 + 1.0, y0 + 3.0, x0 + cell_w - 1.0, y0 + 4.0, fg);
                        image.rect(x0 + 1.0, y0 + ch as f32 - 5.0, x0 + cell_w - 1.0, y0 + ch as f32 - 4.0, fg);
                    }
                } else {
                    let left = x0 + ((cell_w - metrics.advance_width) / 2.0).round() + metrics.xmin as f32;
                    let top = y0 + baseline - (metrics.ymin as f32 + metrics.height as f32);
                    for gy in 0..metrics.height {
                        for gx in 0..metrics.width {
                            let coverage = bitmap[gy * metrics.width + gx];
                            if coverage > 0 {
                                image.blend(left as i64 + gx as i64, top as i64 + gy as i64, fg, coverage as f32 / 255.0);
                            }
                        }
                    }
                }
            }
            if underline {
                image.rect(x0, y0 + font.baseline + 2.0 * scale as f32, x0 + cw as f32, y0 + font.baseline + 3.0 * scale as f32, fg);
            }
        }
    }
    image
}

/// The image as PNG bytes (8-bit RGB, deterministic: no time or text chunks)
pub fn png_bytes(image: &Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, image.width, image.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
        writer.write_image_data(&image.pixels).map_err(|error| error.to_string())?;
        writer.finish().map_err(|error| error.to_string())?;
    }
    Ok(out)
}

/// Writes a screen in a format; returns the bytes written
pub fn write(buf: &Buffer, theme: &Theme, format: Format, scale: u32, path: &Path) -> Result<usize, String> {
    let bytes = encode(buf, theme, format, scale)?;
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    std::fs::write(path, &bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(bytes.len())
}

/// A screen in a format, as bytes
pub fn encode(buf: &Buffer, theme: &Theme, format: Format, scale: u32) -> Result<Vec<u8>, String> {
    Ok(match format {
        Format::Text => text(buf).into_bytes(),
        Format::Svg => svg(buf, theme, scale).into_bytes(),
        Format::Png => {
            let mut font = CellFont::new(scale);
            png_bytes(&raster(buf, theme, &mut font, scale))?
        }
    })
}

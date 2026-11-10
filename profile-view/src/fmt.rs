//! Numbers as the views print them: milliseconds, counts, bytes, shares and changes, each in a fixed form so a
//! rendered screen is the same on every machine.

use crate::metric::Unit;

/// Milliseconds with the precision their size needs: `1.23 s`, `123.4 ms`, `12.34 ms`, `1.234 ms`, `0.123 ms`, `12.3 µs`,
/// and `0 ms` for none
pub fn ms(value: f64) -> String {
    let abs = value.abs();
    if abs == 0.0 {
        "0 ms".into()
    } else if abs >= 1000.0 {
        format!("{:.2} s", value / 1000.0)
    } else if abs >= 100.0 {
        format!("{value:.1} ms")
    } else if abs >= 10.0 {
        format!("{value:.2} ms")
    } else if abs >= 0.1 {
        format!("{value:.3} ms")
    } else if abs >= 0.001 {
        format!("{:.1} µs", value * 1000.0)
    } else {
        format!("{:.0} ns", value * 1e6)
    }
}

/// A whole number with thousands separators: `2,974,531`
pub fn grouped(value: f64) -> String {
    let rounded = value.round();
    let negative = rounded < 0.0;
    let digits = format!("{:.0}", rounded.abs());
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if negative { format!("-{out}") } else { out }
}

/// A count in at most six characters: `789`, `45.6K`, `2.97M`, `1.23G`
pub fn compact(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1e9 {
        format!("{:.2}G", value / 1e9)
    } else if abs >= 1e6 {
        format!("{:.2}M", value / 1e6)
    } else if abs >= 1e4 {
        format!("{:.1}K", value / 1e3)
    } else {
        format!("{value:.0}")
    }
}

/// Bytes: `992 B`, `183.8 KB`, `14.30 MB`, `1.20 GB`
pub fn bytes(value: f64) -> String {
    let abs = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    if abs >= 1024.0 * 1024.0 * 1024.0 {
        format!("{sign}{:.2} GB", abs / (1024.0 * 1024.0 * 1024.0))
    } else if abs >= 1024.0 * 1024.0 {
        format!("{sign}{:.2} MB", abs / (1024.0 * 1024.0))
    } else if abs >= 1024.0 {
        format!("{sign}{:.1} KB", abs / 1024.0)
    } else {
        format!("{sign}{abs:.0} B")
    }
}

/// A part of a whole: `30.1%`, `<0.1%`, `-` when there is no whole
pub fn share(part: f64, whole: f64) -> String {
    if whole <= 0.0 {
        return "-".into();
    }
    let value = part / whole * 100.0;
    if value > 0.0 && value < 0.1 { "<0.1%".into() } else { format!("{value:.1}%") }
}

/// A value in its unit
pub fn value(unit: Unit, value: f64) -> String {
    match unit {
        Unit::Ms => ms(value),
        Unit::Bytes => bytes(value),
        Unit::Count => grouped(value),
        Unit::Share => percent(value),
        Unit::Decimal => decimal(value),
    }
}

/// A value in its unit, short (tables and labels)
pub fn short(unit: Unit, value: f64) -> String {
    match unit {
        Unit::Ms => ms(value),
        Unit::Bytes => bytes(value),
        Unit::Count => compact(value),
        Unit::Share => percent(value),
        Unit::Decimal => decimal(value),
    }
}

/// A part of a whole (0 to 1) as a percentage: `42.0%`
pub fn percent(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

/// A ratio with the decimals its size needs: `0.35`, `12.3`, `1,234`
pub fn decimal(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1000.0 {
        grouped(value)
    } else if abs >= 100.0 {
        format!("{value:.0}")
    } else if abs >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// A change in its unit with its sign: `+1.234 ms`, `-12.0 KB`, `+3`
pub fn signed(unit: Unit, delta: f64) -> String {
    let text = short(unit, delta.abs());
    if delta > 0.0 {
        format!("+{text}")
    } else if delta < 0.0 {
        format!("-{text}")
    } else {
        text
    }
}

/// The change from before to after: `+12.3%`, `-4.0%`, `new`, `gone`, `same`
pub fn change(before: f64, after: f64) -> String {
    let delta = after - before;
    if before.abs() < 1e-12 {
        return if delta.abs() < 1e-12 { "same".into() } else { "new".into() };
    }
    if after.abs() < 1e-12 {
        return "gone".into();
    }
    if delta.abs() < 1e-12 {
        return "same".into();
    }
    format!("{:+.1}%", delta / before * 100.0)
}

/// Text cut to `width` display columns with an ellipsis (every character here is one column wide)
pub fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// Text cut from the left (paths: the end matters): `…/Interface/Grid.luau`
pub fn fit_left(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let skip = count - (width - 1);
    let mut out = String::from("…");
    out.extend(text.chars().skip(skip));
    out
}

/// A label `name  Module:line` cut to `width` columns keeping its line: the name and the module give up columns
/// before the place's end (`:81`, ` (no line)`) does, so `BuildCard  ScrollingGridPresentation:81` in 30 columns
/// is `BuildCard  ScrollingGridPr…:81`. Text without the two spaces is cut at its end.
pub fn fit_label(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let Some(split) = text.rfind("  ") else { return fit(text, width) };
    let (name, place) = (&text[..split], &text[split + 2..]);
    // the place's end: `:81`, or a note in brackets
    let end_at = place.rfind(':').filter(|&at| at + 1 < place.len() && place[at + 1..].bytes().all(|b| b.is_ascii_digit())).or_else(|| place.ends_with(')').then(|| place.rfind(" (")).flatten());
    let (module, end) = end_at.map_or((place, ""), |at| (&place[..at], &place[at..]));
    let (name_len, module_len, end_len) = (name.chars().count(), module.chars().count(), end.chars().count());
    let (min_name, min_module) = (name_len.min(6), module_len.min(4));
    if min_name + 2 + min_module + end_len > width {
        return fit(text, width);
    }
    // what the name and the module share: the name up to half of it, the module the rest, the name what the module
    // leaves
    let room = width - 2 - end_len;
    let name_take = name_len.min((room / 2).max(min_name)).min(room - min_module);
    let module_take = (room - name_take).min(module_len);
    let name_take = (room - module_take).min(name_len);
    format!("{}  {}{end}", fit(name, name_take), fit(module, module_take))
}

/// Pads or cuts to exactly `width` columns, right-aligned
pub fn right(text: &str, width: usize) -> String {
    let text = fit(text, width);
    format!("{text:>width$}")
}

/// Pads or cuts to exactly `width` columns, left-aligned
pub fn left(text: &str, width: usize) -> String {
    let text = fit(text, width);
    format!("{text:<width$}")
}

/// Characters that take more or less than one column (tabs, controls, wide scripts) as one-column stand-ins, so a
/// source line lines up in the grid
pub fn cells(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push(' '),
            c if (c as u32) < 0x1100 => out.push(c),
            // (box drawing, blocks and arrows are one column; wide ideographs and emoji are not)
            c if ('\u{2000}'..='\u{2bff}').contains(&c) => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_the_same_everywhere() {
        assert_eq!(ms(1234.5), "1.23 s");
        assert_eq!(ms(123.45), "123.5 ms");
        assert_eq!(ms(12.345), "12.35 ms");
        assert_eq!(ms(1.2345), "1.234 ms"); // (round half to even on the binary value)
        assert_eq!(ms(0.12345), "0.123 ms");
        assert_eq!(ms(0.012), "12.0 µs");
        assert_eq!(ms(0.0), "0 ms");
        assert_eq!(grouped(2974531.0), "2,974,531");
        assert_eq!(grouped(-1000.0), "-1,000");
        assert_eq!(grouped(12.0), "12");
        assert_eq!(compact(45_600.0), "45.6K");
        assert_eq!(compact(2_974_531.0), "2.97M");
        assert_eq!(bytes(992.0), "992 B");
        assert_eq!(bytes(14.30 * 1024.0 * 1024.0), "14.30 MB");
        assert_eq!(share(1.0, 3.0), "33.3%");
        assert_eq!(share(0.0001, 1.0), "<0.1%");
        assert_eq!(change(10.0, 12.0), "+20.0%");
        assert_eq!(change(0.0, 1.0), "new");
        assert_eq!(change(1.0, 0.0), "gone");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit_left("a/b/c/d.luau", 8), "…/d.luau");
        assert_eq!(fit_label("BuildCard  ScrollingGridPresentation:81", 30), "BuildCard  ScrollingGridPr…:81");
        assert_eq!(fit_label("ReconcileVisibleRowsAndLayout  Lists:413", 20), "Reconcil…  Lists:413");
        assert_eq!(fit_label("anonymous  __index (no line)", 22), "anony…  __i… (no line)");
        assert_eq!(fit_label("rank  Main:21", 30), "rank  Main:21");
        assert_eq!(fit_label("pcall [C] with a long tail", 10), "pcall [C]…");
        assert_eq!(cells("a\tb"), "a    b");
    }
}

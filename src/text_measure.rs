//! TextService:GetTextSize without a text layer (a host layer, roblox-ui-renderer's, measures with real fonts and replaces this): a script
//! that reads `.X` of it ran into nil in a plain `ree run` (a settings window). The answer comes from Studio's own
//! measurements, per Enum.Font (data/text-advances.json, from a text_advances probe): Studio hints its fonts, so a glyph's advance
//! is a whole number of pixels at the size asked and does not scale down from a large size. A least-squares line per character over 23
//! sizes (8-100), `round(a x size + c)` summed over the string, reproduces 76 % of 3,744 sample strings exactly (mean error 0.66 px, 95th
//! percentile 3). A line is `floor(ratio x size)` high (Legacy 1.5, every other font 1.0), a string wider than the frame wraps at spaces
//! (and at newlines), and a word wider than the frame breaks between characters.
//!
//! TextService:GetTextBoundsAsync (a GetTextBoundsParams: Text, Font, Size, Width, RichText) is the same measurement of a Font face
//! (measured in a playing client, Studio 0.747, tools/parity/probes/text_bounds.luau): the bounds of an Enum.Font face are what
//! GetTextSize answers for it, with Width as the frame's width (none when not above 0), but for SourceSansItalic, which GetTextSize
//! measures upright. A face no Enum.Font names measures as the one data/text-faces.json says (tools/text-faces.mjs: a family without the
//! weight or style asked picks a face it has); a face of its own the tables lack, an unknown family and rich text markup are measured
//! as the nearest face, SourceSans and plain text, and reported missing. A client loads a face on its first call, which yields (the
//! faces it starts with do not); a server has no typesetting, so GetTextBoundsAsync raises and GetTextSize answers (0, 0).

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::OnceLock,
};

use mlua::prelude::*;
use serde::Deserialize;

use crate::{
    Engine, K, NameId, Value,
    lua::{Args, Flow, Ret, convert, raise, types::font_to_enum, values},
    scheduler::JobTarget,
    services::{Behaviors, method},
    value::FontValue,
};

#[derive(Deserialize)]
struct Font {
    height: f64,
    a: Vec<f64>,
    c: Vec<f64>,
}

fn fonts() -> &'static HashMap<String, Font> {
    static FONTS: OnceLock<HashMap<String, Font>> = OnceLock::new();
    FONTS.get_or_init(|| serde_json::from_slice(include_bytes!("../data/text-advances.json")).expect("data/text-advances.json"))
}

/// FredokaOne, a font games measure their UI with, size by size (data/fredoka-sizes.json, from a fredoka_sizes probe): Studio's
/// advance of each printable ASCII glyph at every integer
/// size `first`.. (fractional: "l" is 5.5 px at 20), and a line's height. A string is the sum of its advances, rounded once.
#[derive(Deserialize)]
struct SizedFile {
    first: usize,
    heights: Vec<f64>,
    advances: Vec<Vec<f64>>,
    /// per size: `[first code, second code, kern]` for each pair that moves (whole pixels; a space kerns with nothing)
    kerns: Vec<Vec<(u32, u32, f64)>>,
}

struct Sized {
    first: usize,
    heights: Vec<f64>,
    advances: Vec<Vec<f64>>,
    kerns: Vec<HashMap<(u32, u32), f64>>,
}

fn sized() -> &'static Sized {
    static SIZED: OnceLock<Sized> = OnceLock::new();
    SIZED.get_or_init(|| {
        let file: SizedFile = serde_json::from_slice(include_bytes!("../data/fredoka-sizes.json")).expect("data/fredoka-sizes.json");
        let kerns = file.kerns.iter().map(|row| row.iter().map(|&(a, b, kern)| ((a, b), kern)).collect()).collect();
        Sized { first: file.first, heights: file.heights, advances: file.advances, kerns }
    })
}

/// data/text-faces.json (tools/text-faces.mjs, a sweep of every weight and style of the families Enum.Font names, in a playing
/// client): per family file, `"<FontWeight> <FontStyle>"` of a face no Enum.Font names and the Enum.Font face it measures as (`same`),
/// or the one nearest in its widths when it measures as none (`near`, which also holds SourceSansItalic's face: the tables hold
/// GetTextSize's upright answer for it).
#[derive(Deserialize)]
struct FaceFile {
    same: HashMap<String, HashMap<String, String>>,
    near: HashMap<String, HashMap<String, String>>,
}

fn face_file() -> &'static FaceFile {
    static FACES: OnceLock<FaceFile> = OnceLock::new();
    FACES.get_or_init(|| serde_json::from_slice(include_bytes!("../data/text-faces.json")).expect("data/text-faces.json"))
}

/// How GetTextBoundsAsync measures a Font face: with the tables of the Enum.Font face `table`; `exact` when the face measures as that
/// one (else it is the nearest, or SourceSans for a family the tables do not know), and `load`, the face the client loads for it.
pub struct Face {
    pub table: String,
    pub exact: bool,
    pub load: String,
}

pub fn face(engine: &Engine, font: &FontValue) -> Face {
    let reflection = &engine.reflection;
    let family = font.family.to_str_lossy();
    let key = format!("{} {}", reflection.enum_item_info(font.weight).name, reflection.enum_item_info(font.style).name);
    let own = || format!("{family} {key}");
    let file = face_file();
    if let Some(name) = file.near.get(&*family).and_then(|faces| faces.get(&key)) {
        return Face { table: name.clone(), exact: false, load: own() };
    }
    if let Some(item) = font_to_enum(engine, font) {
        let name = &*reflection.enum_item_info(item).name;
        if fonts().contains_key(name) {
            return Face { table: name.to_string(), exact: true, load: own() };
        }
    }
    if let Some(name) = file.same.get(&*family).and_then(|faces| faces.get(&key)) {
        // (the client loads the face it measures as: FredokaOne Bold needs no load once FredokaOne has one)
        let load = match reflection.font_enum_faces.get(name.as_str()) {
            Some((family, weight, style)) => format!("{family} {weight} {style}"),
            None => own(),
        };
        return Face { table: name.clone(), exact: true, load };
    }
    Face { table: "SourceSans".to_string(), exact: false, load: own() }
}

/// The faces a playing client has loaded: the Enum.Font faces it starts with (a call from a metamethod, which cannot yield, measured
/// them; every other face raised "thread is not yieldable"), then those calls loaded.
struct LoadedFaces(RefCell<HashSet<String>>);

impl crate::Extension for LoadedFaces {}

const LOADED_AT_START: &[&str] = &["SourceSans", "SourceSansBold", "SourceSansItalic", "SourceSansLight", "SourceSansSemibold", "Arial", "ArialBold", "Arimo", "ArimoBold"];

/// Marks the face `load` loaded; whether it was already.
fn loaded(engine: &Engine, load: &str) -> bool {
    let faces = engine.extension_or_insert_with(|| {
        let faces = &engine.reflection.font_enum_faces;
        LoadedFaces(RefCell::new(LOADED_AT_START.iter().filter_map(|name| faces.get(*name)).map(|(family, weight, style)| format!("{family} {weight} {style}")).collect()))
    });
    !faces.0.borrow_mut().insert(load.to_string())
}

/// The column of FredokaOne's table for `size`, when the font is FredokaOne and the size is a whole number the table covers.
fn column(font: &str, size: f64) -> Option<usize> {
    let table = sized();
    if font != "FredokaOne" || size.fract() != 0.0 || size < table.first as f64 {
        return None;
    }
    let column = size as usize - table.first;
    (column < table.heights.len()).then_some(column)
}

/// The pixel width of `text` on one line.
fn width(font: &Font, column: Option<usize>, text: &str, size: f64) -> f64 {
    if let Some(column) = column {
        let table = sized();
        let rows = &table.advances;
        let mean = rows.iter().map(|row| row[column]).sum::<f64>() / rows.len() as f64;
        let advances: f64 = text
            .chars()
            .map(|character| match (character as u32).checked_sub(32).filter(|index| *index < 95) {
                Some(index) => rows[index as usize][column],
                None => mean,
            })
            .sum();
        let kerning: f64 = text.chars().zip(text.chars().skip(1)).filter_map(|(a, b)| table.kerns[column].get(&(a as u32, b as u32))).sum();
        return (advances + kerning + 0.5).floor();
    }
    let count = font.a.len() as f64;
    let (mean_a, mean_c) = (font.a.iter().sum::<f64>() / count, font.c.iter().sum::<f64>() / count);
    text.chars()
        .map(|character| {
            let (a, c) = match (character as u32).checked_sub(32).filter(|index| *index < 95) {
                Some(index) => (font.a[index as usize], font.c[index as usize]),
                None => (mean_a, mean_c),
            };
            (a * size + c + 0.5).floor().max(0.0)
        })
        .sum()
}

/// (width, height) of `text` at `size` in `font` (an Enum.Font item's name) inside a frame `limit` wide.
pub fn measure(font: &str, text: &str, size: f64, limit: f64) -> (f64, f64) {
    let table = fonts();
    let column = column(font, size);
    let Some(font) = table.get(font).or_else(|| table.get("SourceSans")) else { return (0.0, size) };
    let line_height = match column {
        Some(column) => sized().heights[column],
        None => (font.height * size + 1e-6).floor(),
    };
    let width = |font: &Font, text: &str, size: f64| width(font, column, text, size);
    let space = width(font, " ", size);
    let mut widest: f64 = 0.0;
    let mut lines = 0.0;
    for paragraph in text.split('\n') {
        lines += 1.0;
        let mut used = 0.0;
        let mut first = true;
        for word in paragraph.split(' ') {
            let wide = width(font, word, size);
            let joined = if first { used + wide } else { used + space + wide };
            if joined <= limit {
                used = joined;
            } else if wide <= limit {
                // (the word starts the next line)
                widest = widest.max(used);
                lines += 1.0;
                used = wide;
            } else {
                // a word wider than the frame breaks between characters (Studio: 62 characters in 40 px at size 14 are 11 lines)
                if !first {
                    widest = widest.max(used);
                    lines += 1.0;
                    used = 0.0;
                }
                for character in word.chars() {
                    let each = width(font, character.encode_utf8(&mut [0; 4]), size);
                    if used + each > limit && used > 0.0 {
                        widest = widest.max(used);
                        lines += 1.0;
                        used = 0.0;
                    }
                    used += each;
                }
            }
            first = false;
        }
        widest = widest.max(used);
    }
    (widest, lines * line_height)
}

pub fn install(engine: &Rc<Engine>, behaviors: &mut Behaviors) {
    let reflection = engine.reflection.clone();
    let Some(class) = reflection.class_id("TextService") else { return };
    if let Some(name) = reflection.name_id("GetTextBoundsAsync") {
        behaviors.add_method(class, name, method(get_text_bounds));
    }
    let Some(name) = reflection.name_id("GetTextSize") else { return };
    behaviors.add_method(class, name, method(|lua, engine, _, args| {
        if engine.options.server() {
            // (a server has no typesetting: every size is (0, 0), measured)
            return Ok(values([convert::to_lua(lua, engine, &Value::Vector2(rbx_dom_weak::types::Vector2::new(0.0, 0.0)))?]));
        }
        let text = args.string(1).map(|text| text.to_string_lossy()).unwrap_or_default();
        let size = args.number(2).unwrap_or(14.0);
        let font = match convert::from_lua(args.get(3)) {
            Value::EnumItem(item) => engine.reflection.enum_item_info(item).name.to_string(),
            _ => "SourceSans".to_string(),
        };
        let limit = match convert::from_lua(args.get(4)) {
            Value::Vector2(frame) if frame.x > 0.0 => frame.x as f64,
            _ => f64::INFINITY,
        };
        engine.state.borrow().sched.charge(crate::cost::TEXT_MEASURED);
        let (x, y) = measure(&font, &text, size, limit);
        let size = Value::Vector2(rbx_dom_weak::types::Vector2::new(x as f32, y as f32));
        Ok(values([convert::to_lua(lua, engine, &size)?]))
    }));
}

/// TextService:GetTextBoundsAsync (measured in a playing client and a server, Studio 0.747: tools/parity/probes/text_bounds.luau).
fn get_text_bounds(lua: &Lua, engine: &Rc<Engine>, service: crate::InstanceId, args: &Args) -> Ret {
    // (`TextService.GetTextBoundsAsync(params)`: the params in the service's place)
    if !engine.reflection.is_a(engine.state.borrow().dom.at(service).class, "TextService") {
        return raise("Expected ':' not '.' calling member function GetTextBoundsAsync");
    }
    let params = match args.get(1) {
        LuaValue::Nil => return raise("Argument 1 missing or nil"),
        value => match crate::lua::instance_id(value) {
            Some(params) => params,
            None => return raise("Unable to cast value to Object"),
        },
    };
    let class = engine.state.borrow().dom.at(params).class;
    if !engine.reflection.is_a(class, "GetTextBoundsParams") {
        return raise("Argument 1 is invalid: expected 'GetTextBoundsParams' instance type");
    }
    if engine.options.server() {
        return raise("Typesetting is not enabled in this DataModel");
    }
    let (text, font, size, width, rich) = {
        let state = engine.state.borrow();
        let get = |name: NameId| state.dom.raw_get(params, name);
        let text = get(K::Text as NameId).and_then(Value::as_str).map(|text| text.to_str_lossy().into_owned()).unwrap_or_default();
        let font = match get(K::Font as NameId) {
            Some(Value::Font(font)) => (**font).clone(),
            _ => crate::lua::types::font_from_enum(engine, engine.reflection.enum_item("Font", "SourceSans").unwrap_or(0)),
        };
        let size = get(K::Size as NameId).and_then(Value::as_number).unwrap_or(20.0);
        let width = engine.reflection.name_id("Width").and_then(get).and_then(Value::as_number).unwrap_or(0.0);
        (text, font, size, width, matches!(get(K::RichText as NameId), Some(Value::Bool(true))))
    };
    if font.family.as_bytes().is_empty() {
        return raise("Font with empty asset ID passed to TextService:GetTextBoundsAsync()");
    }
    let face = face(engine, &font);
    if !face.exact {
        let missing = format!("GetTextBoundsAsync of {} (a face the text tables lack; measured as {})", face.load, face.table);
        engine.missing(Some(lua), "TextService", &missing);
    }
    if rich && text.contains(['<', '&']) {
        // (rich text needs a host layer's parser: Studio reads the tags, entities and <br/>, and raises on markup that does not parse)
        engine.missing(Some(lua), "TextService", "GetTextBoundsAsync of rich text markup (measured as plain text)");
    }
    engine.state.borrow().sched.charge(crate::cost::TEXT_MEASURED);
    // (Size stores 1 to 100, whole: convert::number_for; a value from elsewhere measures within them)
    let size = if size.is_nan() { 1.0 } else { size.trunc().clamp(1.0, 100.0) };
    let limit = if width > 0.0 { width } else { f64::INFINITY };
    let (x, y) = measure(&face.table, &text, size, limit);
    let bounds = Value::Vector2(rbx_dom_weak::types::Vector2::new(x as f32, y as f32));
    if loaded(engine, &face.load) {
        return Ok(values([convert::to_lua(lua, engine, &bounds)?]));
    }
    // The first call of a face waits for it to load (a call that cannot yield raises, and the load goes on: a later call finds it
    // loaded). The client resumes the call when the load is done, which depends on the machine; here at the next resumption point.
    let thread = lua.current_thread();
    if unsafe { mlua::ffi::lua_isyieldable(thread.state()) } == 0 {
        return raise("thread is not yieldable");
    }
    engine.state.borrow_mut().sched.defer(JobTarget::Thread(thread), vec![bounds]);
    Err(Flow::Yield)
}

#[cfg(test)]
mod tests {
    use super::{face_file, fonts, measure};

    /// data/text-faces.json names only faces the tables hold, and no face twice; FredokaOne has one face (every weight and style
    /// measures as it), GothamSSm's SemiBold is a face of its own (measured as no Enum.Font face).
    #[test]
    fn text_faces_measure_as_faces_the_tables_hold() {
        let file = face_file();
        for (family, faces) in file.same.iter().chain(&file.near) {
            for (face, name) in faces {
                assert!(fonts().contains_key(name), "{family} {face}: {name}");
            }
        }
        for (family, faces) in &file.same {
            for face in faces.keys() {
                assert!(!file.near.get(family).is_some_and(|near| near.contains_key(face)), "{family} {face} is in both");
            }
        }
        let fredoka = &file.same["rbxasset://fonts/families/FredokaOne.json"];
        assert_eq!(fredoka.len(), 17);
        assert!(fredoka.values().all(|name| name == "FredokaOne"));
        assert_eq!(file.near["rbxasset://fonts/families/GothamSSm.json"]["SemiBold Normal"], "GothamBold");
        assert_eq!(file.near["rbxasset://fonts/families/SourceSansPro.json"]["Regular Italic"], "SourceSansLight");
    }

    /// Studio's answers (an empty test place, 2026-10-23, the text_advances probe): single lines at sizes 14 and 20 in an unbounded
    /// frame, and the 62 characters of "The quick brown fox jumps over the lazy dog and keeps running" in frames 40-220 px wide.
    /// `(font, text, size, frame, width, height)`.
    const STUDIO: &[(&str, &str, f64, f64, f64, f64)] = &[
        ("FredokaOne", "Hello", 14.0, 1e5, 31.0, 14.0),
        ("FredokaOne", "Hello", 20.0, 1e5, 43.0, 20.0),
        ("FredokaOne", "WWW", 14.0, 1e5, 36.0, 14.0),
        ("FredokaOne", "iii", 14.0, 1e5, 9.0, 14.0),
        ("FredokaOne", "1,234,567", 20.0, 1e5, 76.0, 20.0),
        ("FredokaOne", "Level 12 - Wave 30/40", 14.0, 1e5, 135.0, 14.0),
        ("SourceSans", "Hello", 14.0, 1e5, 26.0, 14.0),
        ("Legacy", "Hello", 14.0, 1e5, 44.0, 21.0),
        ("Legacy", "Hello", 20.0, 1e5, 62.0, 30.0),
        ("SourceSans", "The quick brown fox jumps over the lazy dog and keeps running", 14.0, 40.0, 40.0, 154.0),
        ("SourceSans", "The quick brown fox jumps over the lazy dog and keeps running", 14.0, 100.0, 98.0, 56.0),
        ("SourceSans", "The quick brown fox jumps over the lazy dog and keeps running", 20.0, 100.0, 82.0, 120.0),
        ("SourceSans", "The quick brown fox jumps over the lazy dog and keeps running", 14.0, 220.0, 216.0, 28.0),
        ("FredokaOne", "The quick brown fox jumps over the lazy dog and keeps running", 20.0, 150.0, 138.0, 80.0),
        ("FredokaOne", "The quick brown fox jumps over the lazy dog and keeps running", 14.0, 100.0, 98.0, 56.0),
        ("GothamBold", "The quick brown fox jumps over the lazy dog and keeps running", 20.0, 220.0, 216.0, 60.0),
        ("SourceSans", "Hello world again and again", 20.0, 100.0, 79.0, 60.0),
    ];

    /// TextService:GetTextSize returned nil without a host's text layer (a script reading `.X` failed: the lobby's settings window); it
    /// answers from Studio's own measurements now. Widths within 3 px (the fit's 95th percentile) and the same number of lines.
    #[test]
    fn text_service_measures_text_as_studio_does() {
        for &(font, text, size, frame, width, height) in STUDIO {
            let (x, y) = measure(font, text, size, frame);
            assert!((x - width).abs() <= 3.0, "{font} {text:?} {size} in {frame}: width {x}, Studio {width}");
            assert_eq!(y, height, "{font} {text:?} {size} in {frame}: height {y}, Studio {height}");
        }
    }

    /// FredokaOne at the integer sizes UI uses, from Studio's own table (data/fredoka-sizes.json, the fredoka_sizes probe in an
    /// empty test place, 2026-10-23): a glyph's advance is a whole number of pixels, pairs kern by whole pixels, and a string is the
    /// sum rounded. The per-glyph fit (data/text-advances.json) read 57 % of these samples exactly and was up to 8 px wide on "The quick
    /// brown fox..." at 48; the table reads 694 of 704 exactly (the 10 are 1 px: a same-glyph pair after a kerned pair, "Sett" and "All").
    /// `(text, size, width, height)`, all pinned exactly.
    const FREDOKA: &[(&str, f64, f64, f64)] = &[
        ("Hello", 10.0, 22.0, 10.0),
        ("Hello", 13.0, 29.0, 13.0),
        ("Hello", 15.0, 33.0, 15.0),
        ("Hello", 16.0, 35.0, 16.0),
        ("Hello", 24.0, 51.0, 24.0),
        ("Hello", 36.0, 76.0, 36.0),
        ("Hello", 48.0, 100.0, 48.0),
        ("The quick brown fox jumps over the lazy dog", 10.0, 191.0, 10.0),
        ("The quick brown fox jumps over the lazy dog", 13.0, 252.0, 13.0),
        ("The quick brown fox jumps over the lazy dog", 15.0, 284.0, 15.0),
        ("The quick brown fox jumps over the lazy dog", 16.0, 306.0, 16.0),
        ("The quick brown fox jumps over the lazy dog", 24.0, 443.0, 24.0),
        ("The quick brown fox jumps over the lazy dog", 36.0, 662.0, 36.0),
        ("The quick brown fox jumps over the lazy dog", 48.0, 869.0, 48.0),
        ("Level 12 - Wave 30/40", 10.0, 98.0, 10.0),
        ("Level 12 - Wave 30/40", 13.0, 125.0, 13.0),
        ("Level 12 - Wave 30/40", 15.0, 140.0, 15.0),
        ("Level 12 - Wave 30/40", 16.0, 155.0, 16.0),
        ("Level 12 - Wave 30/40", 24.0, 222.0, 24.0),
        ("Level 12 - Wave 30/40", 36.0, 331.0, 36.0),
        ("Level 12 - Wave 30/40", 48.0, 434.0, 48.0),
        ("Wa Yo Ty AV", 10.0, 52.0, 10.0),
        ("Wa Yo Ty AV", 13.0, 71.0, 13.0),
        ("Wa Yo Ty AV", 15.0, 78.0, 15.0),
        ("Wa Yo Ty AV", 16.0, 85.0, 16.0),
        ("Wa Yo Ty AV", 24.0, 123.0, 24.0),
        ("Wa Yo Ty AV", 36.0, 184.0, 36.0),
        ("Wa Yo Ty AV", 48.0, 244.0, 48.0),
        ("Join Match", 10.0, 48.0, 10.0),
        ("Join Match", 13.0, 61.0, 13.0),
        ("Join Match", 15.0, 70.0, 15.0),
        ("Join Match", 16.0, 75.0, 16.0),
        ("Join Match", 24.0, 109.0, 24.0),
        ("Join Match", 36.0, 163.0, 36.0),
        ("Join Match", 48.0, 213.0, 48.0),
        ("Unit Limit: 6/6", 10.0, 61.0, 10.0),
        ("Unit Limit: 6/6", 13.0, 78.0, 13.0),
        ("Unit Limit: 6/6", 15.0, 88.0, 15.0),
        ("Unit Limit: 6/6", 16.0, 97.0, 16.0),
        ("Unit Limit: 6/6", 24.0, 141.0, 24.0),
        ("Unit Limit: 6/6", 36.0, 210.0, 36.0),
        ("Unit Limit: 6/6", 48.0, 275.0, 48.0),
    ];

    #[test]
    fn fredoka_one_is_exact_at_the_sizes_the_game_measures() {
        for &(text, size, width, height) in FREDOKA {
            let (x, y) = measure("FredokaOne", text, size, 1e5);
            assert_eq!((x, y), (width, height), "FredokaOne {text:?} at {size}: {x} x {y}, Studio {width} x {height}");
        }
        // a size the table does not hold (a fraction) falls back to the per-glyph fit
        let (x, _) = measure("FredokaOne", "Hello", 14.5, 1e5);
        assert!((x - 33.0).abs() <= 3.0, "{x}");
    }
}

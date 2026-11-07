//! Every view and the key paths through them, drawn headless at a fixed size and compared with snapshots
//! (tests/snapshots; REE_VIEW_BLESS=1 rewrites them), and the files `render` writes: text, SVG and PNG.

mod common;

use common::*;
use ree_profile_view::{
    app::View,
    cli,
    export::{self, CellFont, Format},
    theme::{Mark, Theme, ThemeKind},
};

const COLS: u16 = 120;
const ROWS: u16 = 40;

#[test]
fn every_view_matches_its_snapshot() {
    let temp = Temp::new("views");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    for view in [View::Overview, View::Flame, View::Calls, View::Source, View::Alloc, View::Heap, View::Timeline, View::Caches, View::Issues] {
        let mut app = open(&[&dir]);
        app.view = view;
        let text = screen(&mut app, "", COLS, ROWS);
        snapshot(&format!("view-{}", view.name()), &text, temp.path());
    }
}

/// (snapshot, key script): each a path a user takes, from the Overview
const PATHS: &[(&str, &str)] = &[
    // the first issue (a sort every frame) opens its function in Calls
    ("overview-enter", "enter"),
    // the flame starts on the heaviest phase; down its heaviest callees twice, enter zooms in there, esc back out
    ("flame-zoom", "2 down down enter"),
    ("flame-zoom-out", "2 down down enter esc"),
    // Calls on rank (found by name): down to its callee (sort), enter centres on it, esc back to rank
    ("calls-centre", "3 /rank down down enter"),
    ("calls-back", "3 /rank down down enter esc"),
    // Source lists lines by heat with s
    ("source-hot", "4 s"),
    // Allocations sorted by bytes a frame in the steady state, then enter opens the site's line
    ("alloc-steady", "5 s s"),
    ("alloc-enter", "5 enter"),
    // the Heap's next pane and the snapshot before the last
    ("heap-pane", "6 tab left"),
    // the Timeline: three frames left of the worst one, then zoomed in
    ("timeline-move", "7 left left left"),
    ("timeline-zoom", "7 enter"),
    // Caches by miss rate
    ("caches-rate", "8 s"),
    // Issues: the second one opened
    ("issues-open", "9 down enter"),
    // the metric switched to allocated bytes on the Flame, and to instructions on the Overview
    ("flame-alloc", "2 m m m"),
    ("overview-insns", "m"),
    // a search in the Flame finds compare's node
    ("flame-search", "2 /anonymous n"),
    // scripts only folds C functions into their callers
    ("flame-scripts", "2 f"),
    // the help over the Flame
    ("flame-help", "2 ?"),
    // the light theme draws the same text (colours are the PNG test's)
    ("overview-light", "t"),
];

#[test]
fn key_paths_match_their_snapshots() {
    let temp = Temp::new("paths");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    for (name, keys) in PATHS {
        let mut app = open(&[&dir]);
        let text = screen(&mut app, keys, COLS, ROWS);
        snapshot(&format!("path-{name}"), &text, temp.path());
    }
}

#[test]
fn a_pair_opens_on_the_diff_and_its_tables() {
    let temp = Temp::new("pair");
    let before = write_profile(&temp.path().join("before"), Shape::BEFORE);
    let after = write_profile(&temp.path().join("after"), Shape::AFTER);
    for (name, keys) in [("diff-functions", ""), ("diff-lines", "tab"), ("diff-sites", "tab tab"), ("diff-grew", "s"), ("diff-enter", "enter"), ("diff-side", "x 1")] {
        let mut app = open(&[&before, &after]);
        assert_eq!(app.view, View::Diff, "a pair starts on the diff");
        let text = screen(&mut app, keys, COLS, ROWS);
        snapshot(&format!("pair-{name}"), &text, temp.path());
    }
}

#[test]
fn a_comparison_opens_on_compare_and_its_runs_open() {
    let temp = Temp::new("compare");
    let dir = write_comparison(&temp.path().join("what-if"));
    for (name, keys) in [("compare", ""), ("compare-alloc", "m m"), ("compare-size", "left down"), ("compare-open", "down enter"), ("compare-diff", "down D"), ("compare-issues", "9")] {
        let mut app = open(&[&dir]);
        assert_eq!(app.view, View::Compare, "a what-if folder starts on Compare");
        let text = screen(&mut app, keys, COLS, ROWS);
        snapshot(&format!("compare-{name}"), &text, temp.path());
    }
    // the baseline's run at the largest size is open in the other views
    let mut app = open(&[&dir]);
    assert!(app.has_profile());
    assert_eq!(app.profile().raw.run.frames, 120);
    // enter on a run with no profile folder says so and stays
    let text = screen(&mut app, "left enter", COLS, ROWS);
    assert_eq!(app.view, View::Compare, "{text}");
    assert!(text.contains("no profile folder"), "{text}");
}

#[test]
fn the_same_screen_draws_the_same_bytes() {
    let temp = Temp::new("determinism");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let draw = || {
        let mut app = open(&[&dir]);
        app.view = View::Flame;
        cli::screen(&mut app, COLS, ROWS)
    };
    let (a, b) = (draw(), draw());
    let theme = Theme::new(ThemeKind::Dark);
    for format in [Format::Png, Format::Svg, Format::Text] {
        assert_eq!(export::encode(&a, &theme, format, 1).unwrap(), export::encode(&b, &theme, format, 1).unwrap(), "{format:?}");
    }
}

/// The PNG is the cells rasterized: its size is the grid's, a cell's background and a bar's colour are where the buffer
/// puts them, and a bar is drawn as a band through the middle of its cell (a gap above and below)
#[test]
fn the_png_is_the_buffer_rasterized() {
    let temp = Temp::new("png");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let mut app = open(&[&dir]);
    let buf = cli::screen(&mut app, COLS, ROWS);
    let theme = app.theme.clone();
    let bytes = export::encode(&buf, &theme, Format::Png, 1).unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    let (cw, ch) = (export::CELL_W, export::CELL_H);
    assert_eq!((info.width, info.height), (COLS as u32 * cw, ROWS as u32 * ch));
    let pixel = |x: u32, y: u32| -> (u8, u8, u8) {
        let at = ((y * info.width + x) * 3) as usize;
        (pixels[at], pixels[at + 1], pixels[at + 2])
    };
    let rgb = |color: ratatui::style::Color| match color {
        ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
        _ => panic!("the themes use RGB colours"),
    };
    // the brand's pill, at the first cell's corner
    assert_eq!(pixel(0, 0), rgb(theme.accent));
    // a full bar cell: the bar's colour across the middle of the cell, its row's background above and below it
    let (bx, by) = (0..ROWS)
        .flat_map(|y| (0..COLS).map(move |x| (x, y)))
        .find(|&(x, y)| buf[(x, y)].symbol() == "█" && Theme::unmark(buf[(x, y)].bg).1 == Mark::Bar)
        .expect("a bar on the Overview");
    let fg = rgb(buf[(bx, by)].fg);
    let row_bg = rgb(Theme::unmark(buf[(bx, by)].bg).0);
    let (x0, y0) = (bx as u32 * cw, by as u32 * ch);
    assert_eq!(pixel(x0 + cw / 2, y0 + ch / 2), fg, "the bar's middle");
    assert_eq!(pixel(x0 + cw / 2, y0 + 1), row_bg, "a gap above the bar");
    assert_eq!(pixel(x0 + cw / 2, y0 + ch - 2), row_bg, "a gap below the bar");
    // a letter's cell has ink in it
    let (lx, ly) = (0..COLS).map(|x| (x, 0)).find(|&(x, y)| buf[(x, y)].symbol() == "r").unwrap();
    let ink = rgb(buf[(lx, ly)].fg);
    let inked = (0..cw).flat_map(|dx| (0..ch).map(move |dy| (dx, dy))).filter(|&(dx, dy)| pixel(lx as u32 * cw + dx, ly as u32 * ch + dy) == ink).count();
    assert!(inked >= 3, "the r of the header is drawn ({inked} pixels in ink)");
    // and twice as large at scale 2
    let bytes = export::encode(&buf, &theme, Format::Png, 2).unwrap();
    let info = png::Decoder::new(std::io::Cursor::new(&bytes)).read_info().unwrap().info().clone();
    assert_eq!((info.width, info.height), (COLS as u32 * cw * 2, ROWS as u32 * ch * 2));
}

/// Every character any view draws is in the bundled font, or drawn as a shape (blocks, box lines, braille)
#[test]
fn every_character_the_views_draw_has_a_glyph() {
    let temp = Temp::new("glyphs");
    let before = write_profile(&temp.path().join("before"), Shape::BEFORE);
    let after = write_profile(&temp.path().join("after"), Shape::AFTER);
    let what_if = write_comparison(&temp.path().join("what-if"));
    let mut seen = std::collections::BTreeSet::new();
    let mut take = |app: &mut ree_profile_view::app::App, keys: &str| {
        let text = screen(app, keys, COLS, ROWS);
        seen.extend(text.chars().filter(|c| *c != '\n'));
    };
    for view in View::ALL {
        let mut app = open(&[&before]);
        if app.available(view) {
            app.view = view;
            take(&mut app, "");
            take(&mut app, "?");
        }
    }
    let mut pair = open(&[&before, &after]);
    take(&mut pair, "");
    take(&mut pair, "tab tab");
    let mut compare = open(&[&what_if]);
    take(&mut compare, "");
    take(&mut compare, "9");
    let font = CellFont::new(1);
    let missing: Vec<char> = seen.into_iter().filter(|c| !font.covers(*c)).collect();
    assert!(missing.is_empty(), "characters the font lacks: {missing:?}");
}

#[test]
fn render_writes_a_file_a_view_and_prints_each() {
    let temp = Temp::new("render");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let out = temp.path().join("shots");
    let args: Vec<String> = [dir.to_str().unwrap(), "--view", "all", "--out", out.to_str().unwrap(), "--size", "100x30"].iter().map(|s| s.to_string()).collect();
    assert_eq!(cli::render("ree profile", &args), 0);
    for view in ["overview", "flame", "calls", "source", "alloc", "heap", "timeline", "caches", "issues"] {
        let file = out.join(format!("{view}.png"));
        assert!(file.is_file(), "{}", file.display());
    }
    assert!(!out.join("diff.png").exists(), "one profile has no diff");
    // one view to text and to SVG, by the file's extension
    for (file, check) in [("issues.txt", "Sorted every frame"), ("flame.svg", "<svg ")] {
        let path = temp.path().join(file);
        let view = file.split('.').next().unwrap();
        let args: Vec<String> = [dir.to_str().unwrap(), "--view", view, "--out", path.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
        assert_eq!(cli::render("ree profile", &args), 0);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(check), "{file}: {text}");
    }
    // what it refuses, before it writes anything
    for (args, error) in [
        (vec![dir.to_str().unwrap(), "--view", "flame"], "--out"),
        (vec![dir.to_str().unwrap(), "--view", "diff", "--out", "x.png"], "two profiles"),
        (vec![dir.to_str().unwrap(), "--view", "nope", "--out", "x.png"], "no view"),
        (vec![dir.to_str().unwrap(), "--metric", "nope", "--out", "x.png"], "no metric"),
        (vec![dir.to_str().unwrap(), "--select", "nothing_like_this", "--out", "x.png"], "--select"),
        (vec![dir.to_str().unwrap(), "--view", "all", "--out", "x.png"], "a folder"),
        (vec![dir.to_str().unwrap(), "--size", "10x5", "--out", "x.png"], "--size"),
        (vec![temp.path().join("nothing").to_str().unwrap(), "--out", "x.png"], "no profile in"),
    ] {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        assert_eq!(cli::render("ree profile", &args), 2, "{args:?} ({error})");
    }
    assert!(!std::path::Path::new("x.png").exists());
}

#[test]
fn select_finds_a_function_or_a_line() {
    let temp = Temp::new("select");
    let dir = write_profile(&temp.path().join("run"), Shape::BEFORE);
    let mut app = open(&[&dir]);
    assert!(app.select("layout"));
    let focus = app.focus.unwrap();
    assert_eq!(app.profile().funcs[focus.func as usize].name, "layout");
    let mut app = open(&[&dir]);
    assert!(app.select("Main.luau:21"));
    let focus = app.focus.unwrap();
    assert_eq!((app.profile().funcs[focus.func as usize].name.as_str(), focus.line), ("rank", Some(21)));
    app.view = View::Source;
    let text = screen(&mut app, "", COLS, ROWS);
    assert!(text.lines().any(|line| line.contains("▌") && line.contains("21")), "the line is selected:\n{text}");
    assert!(!open(&[&dir]).select("nothing_like_this"));
}

#[test]
fn the_games_code_leads_and_the_harnesss_comes_apart() {
    use ree_profile_view::ui::overview::{Side, sides};
    let temp = Temp::new("sides");
    let dir = write_profile(&temp.path().join("run"), Shape::HARNESS);
    let mut app = open(&[&dir]);
    let text = screen(&mut app, "", COLS, ROWS);
    snapshot("harness-overview", &text, temp.path());
    assert!(text.contains("The game's code") && text.contains("The harness's code"), "{text}");
    // the steady phase's self work: the game's code 10.63 ms (rank's call from the harness is the game's: a script
    // function is its module's side), the harness's 4.000 ms (its scan, a C function, goes with the harness)
    let labels = |app: &ree_profile_view::app::App, side: &Side| side.rows.iter().map(|r| (app.profile().funcs[r.func as usize].label(), (r.own * 1e6).round() / 1e6)).collect::<Vec<_>>();
    let got = sides(&app);
    assert!(got.apart);
    assert!((got.lead.total - 10.63).abs() < 1e-9 && (got.harness.total - 4.0).abs() < 1e-9, "{} {}", got.lead.total, got.harness.total);
    assert_eq!(labels(&app, &got.harness), [("GetDescendants [C]".to_string(), 2.5), ("watch  Fixture:5".to_string(), 1.5)]);
    assert_eq!(labels(&app, &got.lead)[..3], [("anonymous  Main:22".to_string(), 4.0), ("rank  Main:20".to_string(), 3.03), ("layout  Main:30".to_string(), 2.0)]);
    // a profile from before ree c692e40 (no harness flags, no figures per phase, no scope) reads the same from its call
    // tree, its steady phase and the harness's folder
    let path = dir.join("profile.json");
    let mut json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for f in json["functions"].as_array_mut().unwrap() {
        let f = f.as_object_mut().unwrap();
        f.remove("harness");
        f.remove("phases");
    }
    json.as_object_mut().unwrap().remove("scope");
    let old = temp.path().join("old");
    std::fs::create_dir_all(&old).unwrap();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), old.join(entry.file_name())).unwrap();
    }
    std::fs::write(old.join("profile.json"), json.to_string()).unwrap();
    let older = open(&[&old]);
    let from_tree = sides(&older);
    assert!(from_tree.apart);
    assert_eq!((from_tree.lead.total * 1e6).round(), (got.lead.total * 1e6).round());
    assert_eq!(labels(&older, &from_tree.harness), labels(&app, &got.harness));
    assert_eq!(labels(&older, &from_tree.lead), labels(&app, &got.lead));
}

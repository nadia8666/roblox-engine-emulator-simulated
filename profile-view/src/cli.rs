//! `<program> view` and `<program> render`: the command lines (ree's `ree profile view|render`, rur's `rur profile
//! view|render`), their options, and opening what they name.

use std::path::{Path, PathBuf};

use ratatui::{buffer::Buffer, layout::Rect};

use crate::{
    app::{App, View},
    compare::Compare,
    export::{self, Format},
    keys,
    metric::Metric,
    model::Profile,
    theme::ThemeKind,
    ui,
};

/// The default screen size of `render` (columns x rows)
pub const DEFAULT_SIZE: (u16, u16) = (160, 48);

pub fn usage(program: &str) -> String {
    format!(
        "usage: {program} view <dir> [<dir2>] [--view <name>] [--metric <m>] [--select <function|file:line>] [--theme dark|light]
       {program} render <dir> [<dir2>] --out <file.png|.svg|.txt|dir|-> [--view <name>|all|<a,b>] [--metric <m>]
                 [--select <function|file:line>] [--keys \"<keys>\"] [--size <cols>x<rows>] [--theme dark|light] [--scale <n>]
                 [--format png|svg|txt]
<dir>: a profile folder (it holds profile.json), a run's folder with one in profile/, or a what-if folder (compare.json);
       <dir2>: the after of a before/after pair
views: overview, flame, calls, source, alloc, heap, timeline, caches, issues, diff (a pair), compare (a what-if folder)
metrics: work, time, insns, alloc (bytes), allocs (count), misses (Luau caches), or a hardware counter's name
keys (--keys): characters as typed, and enter esc backspace up down left right pgup pgdn home end tab backtab space;
       /text searches for text"
    )
}

/// Parsed options
#[derive(Debug, Default)]
pub struct Options {
    pub paths: Vec<PathBuf>,
    pub view: Option<String>,
    pub metric: Option<String>,
    pub select: Option<String>,
    pub keys: Option<String>,
    pub size: Option<(u16, u16)>,
    pub theme: Option<ThemeKind>,
    pub out: Option<String>,
    pub scale: u32,
    pub format: Option<Format>,
}

fn parse_size(text: &str) -> Option<(u16, u16)> {
    let (cols, rows) = text.split_once(['x', 'X'])?;
    let (cols, rows) = (cols.trim().parse::<u16>().ok()?, rows.trim().parse::<u16>().ok()?);
    (cols >= ui::MIN_WIDTH && rows >= ui::MIN_HEIGHT && cols <= 1000 && rows <= 500).then_some((cols, rows))
}

pub fn parse(args: &[String], render: bool) -> Result<Options, String> {
    let mut options = Options { scale: 1, ..Options::default() };
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(name) = arg.strip_prefix("--") {
            let value = args.get(index + 1).cloned().ok_or_else(|| format!("--{name} takes a value"))?;
            index += 2;
            match name {
                "view" | "views" => options.view = Some(value),
                "metric" => options.metric = Some(value),
                "select" => options.select = Some(value),
                "theme" => options.theme = Some(ThemeKind::parse(&value).ok_or_else(|| format!("--theme: dark or light, not {value}"))?),
                "keys" if render => options.keys = Some(value),
                "size" if render => options.size = Some(parse_size(&value).ok_or_else(|| format!("--size: <cols>x<rows>, at least {}x{} (not {value})", ui::MIN_WIDTH, ui::MIN_HEIGHT))?),
                "out" if render => options.out = Some(value),
                "scale" if render => options.scale = value.parse::<u32>().ok().filter(|s| (1..=4).contains(s)).ok_or_else(|| format!("--scale: 1 to 4, not {value}"))?,
                "format" if render => options.format = Some(Format::parse(&value).ok_or_else(|| format!("--format: png, svg or txt, not {value}"))?),
                _ => return Err(format!("unknown option --{name}")),
            }
        } else {
            options.paths.push(PathBuf::from(arg));
            index += 1;
        }
    }
    if options.paths.is_empty() || options.paths.len() > 2 {
        return Err("give one profile folder, or two (before and after)".into());
    }
    if render && options.out.is_none() {
        return Err("render needs --out <file.png|.svg|.txt|dir|->".into());
    }
    Ok(options)
}

/// What a path names
pub enum Input {
    Profile(PathBuf),
    Compare(PathBuf),
}

pub fn resolve(path: &Path) -> Result<Input, String> {
    if path.is_file() {
        return match path.file_name().and_then(|n| n.to_str()) {
            Some("compare.json") => Ok(Input::Compare(path.parent().unwrap_or(Path::new(".")).to_path_buf())),
            Some("profile.json") => Ok(Input::Profile(path.parent().unwrap_or(Path::new(".")).to_path_buf())),
            _ => Err(format!("{}: give a profile folder, a run's folder or a what-if folder", path.display())),
        };
    }
    if path.join("profile.json").is_file() {
        return Ok(Input::Profile(path.to_path_buf()));
    }
    if path.join("compare.json").is_file() {
        return Ok(Input::Compare(path.to_path_buf()));
    }
    if path.join("profile").join("profile.json").is_file() {
        return Ok(Input::Profile(path.join("profile")));
    }
    Err(format!("no profile in {}: give a profile folder (it holds profile.json), the folder of a run made with --profile (profile/ in it), or a what-if folder (compare.json)", path.display()))
}

/// The App for the paths: one profile, a pair, or a comparison (with its baseline's run at the largest size shown)
pub fn open(paths: &[PathBuf], theme: ThemeKind) -> Result<App, String> {
    let mut profiles = Vec::new();
    let mut compare = None;
    for path in paths {
        match resolve(path)? {
            Input::Profile(dir) => profiles.push(Profile::load(&dir)?),
            Input::Compare(dir) => {
                if paths.len() > 1 {
                    return Err("a what-if folder opens on its own (its runs pair up in Compare: D diffs one against the baseline)".into());
                }
                let c = Compare::load(&dir)?;
                let largest = c.sizes.iter().copied().fold(0.0, f64::max);
                if let Some(dir) = c.run(&c.baseline, largest).and_then(|run| c.profile_dir(run)) {
                    profiles.push(Profile::load(&dir)?);
                }
                compare = Some(c);
            }
        }
    }
    let pair = profiles.len() == 2;
    let comparison = compare.is_some();
    let mut app = App::new(profiles, compare, theme);
    if comparison {
        app.view = View::Compare;
    } else if pair {
        app.view = View::Diff;
    }
    Ok(app)
}

/// Applies --view (one view), --metric and --select
fn apply(app: &mut App, options: &Options, view: Option<View>) -> Result<(), String> {
    if let Some(name) = &options.metric {
        if app.view == View::Compare && !app.has_profile() {
            app.compare_view.metric = Some(name.clone());
        } else if !app.has_profile() {
            return Err("--metric: no profile loaded".into());
        } else {
            let metric = Metric::parse(name, app.profile()).ok_or_else(|| {
                let p = app.profile();
                let names: Vec<String> = Metric::available(p).iter().map(|m| m.cli_name(p)).collect();
                format!("--metric: no metric \"{name}\" in this profile; it has {}", names.join(", "))
            })?;
            if !Metric::available(app.profile()).contains(&metric) {
                return Err(format!("--metric: this profile has no {}", metric.label(app.profile())));
            }
            app.set_metric(metric);
        }
        // (a comparison's own metrics go by their compare.json key)
        if let Some(compare) = &app.compare {
            if compare.measured().iter().any(|m| &m.key == name) {
                app.compare_view.metric = Some(name.clone());
            }
        }
    }
    if let Some(query) = &options.select {
        if !app.select(query) {
            return Err(format!("--select: no function or line matches \"{query}\" (a function's name, Module:line or file.luau:line)"));
        }
    }
    if let Some(view) = view {
        if !app.available(view) {
            return Err(match view {
                View::Diff => "--view diff needs two profiles".to_string(),
                View::Compare => "--view compare needs a what-if folder (compare.json)".to_string(),
                _ => "no profile loaded".to_string(),
            });
        }
        app.view = view;
    }
    Ok(())
}

/// The views `--view` names: one, a list, or `all` (every view the input has)
fn views(app: &App, spec: Option<&str>) -> Result<Vec<Option<View>>, String> {
    let Some(spec) = spec else { return Ok(vec![None]) };
    if spec.trim().eq_ignore_ascii_case("all") {
        return Ok(View::ALL.iter().copied().filter(|v| app.available(*v)).map(Some).collect());
    }
    spec.split(',').map(|name| View::parse(name).map(Some).ok_or_else(|| format!("--view: no view \"{}\"; the views: {}", name.trim(), View::ALL.iter().map(|v| v.name()).collect::<Vec<_>>().join(", ")))).collect()
}

/// Draws the screen as the terminal would, `cols` x `rows`
pub fn screen(app: &mut App, cols: u16, rows: u16) -> Buffer {
    let area = Rect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);
    ui::draw_buffer(&mut buf, area, app);
    buf
}

/// Plays a key script as the terminal does: a draw, then each key followed by a draw
pub fn play(app: &mut App, script: &str, cols: u16, rows: u16) -> Result<(), String> {
    let keys = keys::parse_script(script)?;
    screen(app, cols, rows);
    for key in keys {
        app.key(key);
        if app.quit {
            break;
        }
        screen(app, cols, rows);
    }
    app.quit = false;
    Ok(())
}

/// `view`: the terminal UI
pub fn view(program: &str, args: &[String]) -> i32 {
    let options = match parse(args, false) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{program} view: {error}\n{}", usage(program));
            return 2;
        }
    };
    let mut app = match open(&options.paths, options.theme.unwrap_or(ThemeKind::Dark)) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("{program} view: {error}");
            return 2;
        }
    };
    let view = match views(&app, options.view.as_deref()) {
        Ok(views) if views.len() == 1 => views[0],
        Ok(_) => {
            eprintln!("{program} view: --view takes one view here");
            return 2;
        }
        Err(error) => {
            eprintln!("{program} view: {error}");
            return 2;
        }
    };
    if let Err(error) = apply(&mut app, &options, view) {
        eprintln!("{program} view: {error}");
        return 2;
    }
    match crate::terminal::run(&mut app) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{program} view: {error}");
            1
        }
    }
}

/// `render`: the screens as files, headless. Prints each file written (and its Markdown image line for a PNG)
pub fn render(program: &str, args: &[String]) -> i32 {
    match render_inner(args) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            0
        }
        Err(error) => {
            eprintln!("{program} render: {error}");
            if error.starts_with("give ") || error.starts_with("render needs") || error.starts_with("unknown option") || error.contains("takes a value") {
                eprintln!("{}", usage(program));
            }
            2
        }
    }
}

fn render_inner(args: &[String]) -> Result<Vec<String>, String> {
    let options = parse(args, true)?;
    let (cols, rows) = options.size.unwrap_or(DEFAULT_SIZE);
    let theme = options.theme.unwrap_or(ThemeKind::Dark);
    let mut app = open(&options.paths, theme)?;
    let wanted = views(&app, options.view.as_deref())?;
    let out = options.out.clone().unwrap_or_default();
    let to_stdout = out == "-";
    let out_path = PathBuf::from(&out);
    // one file, or a folder with a file a view
    let single_file = !to_stdout && Format::of_path(&out_path).is_some();
    if single_file && wanted.len() > 1 {
        return Err(format!("--out {out}: several views need a folder (each view goes in <folder>/<view>.<format>)"));
    }
    let format = if to_stdout { options.format.unwrap_or(Format::Text) } else if single_file { Format::of_path(&out_path).unwrap() } else { options.format.unwrap_or(Format::Png) };
    let mut lines = Vec::new();
    let mut printed = String::new();
    // (the metric, the selection and the first view, then the keys, once; the other views are drawn in the state the
    // keys left)
    apply(&mut app, &options, wanted.first().copied().flatten())?;
    if let Some(script) = &options.keys {
        play(&mut app, script, cols, rows)?;
    }
    for (index, view) in wanted.iter().enumerate() {
        if let (true, Some(view)) = (index > 0, view) {
            app.view = *view;
        }
        let buf = screen(&mut app, cols, rows);
        if to_stdout {
            if format != Format::Text {
                return Err("--out - writes text: --format txt".into());
            }
            printed.push_str(&export::text(&buf));
            continue;
        }
        let path = if single_file { out_path.clone() } else { out_path.join(format!("{}.{}", app.view.name(), format.extension())) };
        export::write(&buf, &app.theme, format, options.scale, &path)?;
        let shown = absolute(&path);
        lines.push(if format == Format::Png { format!("{shown}  ![{}]({})", app.view.title(), markdown_target(&shown)) } else { shown });
    }
    if to_stdout {
        print!("{printed}");
    }
    Ok(lines)
}

fn absolute(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").unwrap_or(&text).replace('\\', "/")
}

/// A Markdown link target: in angle brackets when the path has spaces
fn markdown_target(path: &str) -> String {
    if path.contains(' ') { format!("<{path}>") } else { path.to_string() }
}

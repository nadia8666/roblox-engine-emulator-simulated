//! What a command line asks of an engine: the engine options every command takes, read from
//! arguments ([`parse`], or [`Args::accept`] inside a host's own parser), and the [`Options`],
//! project roots and mounts they come to ([`prepare`]).
//!
//!   [--roots "<dir>;<dir>"] [--project <dir>] [--place <place name>]
//!   [--mount Service/Name=<dir|file>]... [--instance Service/Name=<ClassName>]...
//!   [--rojo <file.project.json> [--rojo-at Service/Name]]
//!   [--snapshot <file.rbxm>]... [--snapshot-dir <dir>] [--no-snapshots] [--viewport WxH]
//!   [--scale S] [--camera WxH] [--rotation-clipping enabled|disabled] [--mode edit|play]
//!   [--run-context client|server|loopback] [--player Name] [--place-id N] [--streaming R] [--offline]
//!   [--verbose] [--repo <dir>]
//!
//! The project's Rojo roots are `--roots`; without them there is no project (a run of the
//! harness and its mounts alone), and `--project` only names the project's folder.
//! `--mount` / `--instance` (in order, after the roots) place a directory, file or instance at an
//! instance path (roblox-ui-renderer's cli/rojo.mjs translates Rojo project files into them); with
//! mounts and no `--roots` there is no implicit project. `--rojo <file.project.json>` mounts a Rojo
//! project file's tree where the roots would mount ([`crate::rojo`]; a tree that is not a DataModel at
//! ReplicatedStorage/<project name>, or at `--rojo-at Service/Name`), before the `--mount`s; it takes the
//! place of `--roots` (the two are refused together), and its folder is the place's root for what is
//! looked up by a root ([`crate::mount::ProjectSpec::place_roots`]). Content naming a relative local path
//! (`output/x.mesh`) is read from the project's folder: `--project`, else the folder holding the `--rojo`
//! file, else the folder holding the first root, else the current directory ([`Options::project`]).
//! Environment: RUR_CAMERA / RUR_PLACE_ID /
//! RUR_PLAYER / RUR_PLAYER_ID / RUR_PLAYER_DISPLAY / RUR_ROTATION_CLIPPING / RUR_VERBOSE stand in
//! for the flags, and REE_LOCALIZED_PLACES / REE_MATCH_PLACES add places to the work clock's place
//! prices ([`crate::cost::PlacePrices::with`]).

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    rc::Rc,
};

use super::{Engine, Options, RunContext, project};

/// RUR_STATE_DIR / RUR_CACHE_DIR when relative, made absolute (from the current directory): a service
/// runs in its state dir, where a relative one would name another directory.
fn relative_state_variables() -> Vec<(&'static str, PathBuf)> {
    ["RUR_STATE_DIR", "RUR_CACHE_DIR"]
        .into_iter()
        .filter_map(|name| {
            let value = std::env::var_os(name).filter(|value| !value.is_empty() && Path::new(value).is_relative())?;
            Some((name, std::path::absolute(value).ok()?))
        })
        .collect()
}

/// Environment variables naming a file or folder the client chose: the recorded clock, the engine dump and the profile
/// outputs. A service runs in its state dir, where a relative value would name another file than the client meant.
const CLIENT_PATH_VARIABLES: [&str; 6] =
    ["RUR_ENGINE_DUMP", "RUR_CLOCK", "RUR_BOUNDARY_PROFILE", "RUR_LUAU_PROFILE", "RUR_LUAU_ALLOC_PROFILE", "REE_PROFILE_OUT"];

/// The per-user cache roots and temporary folders. The OS, Rust's temp-folder lookup and child processes read them, so
/// the service must see the client's folders, and two clients in different folders must not share a service key.
const USER_FOLDER_VARIABLES: [&str; 8] = ["LOCALAPPDATA", "HOME", "XDG_CACHE_HOME", "APPDATA", "USERPROFILE", "TMP", "TEMP", "TMPDIR"];

/// The absolute form of a path the client passes to a service or worker, or `None` when it cannot be made absolute.
///
/// The cache lookup ignores a relative per-user folder, but a service or worker runs in the state dir, where the same
/// relative spelling would name another folder than the client meant. So these values are resolved against the client's
/// current folder before they are forwarded. That cannot turn an ignored folder into the cache: the state dir is pinned
/// by then (see `resolve_standalone_state`), and a later lookup returns the pinned folder.
pub(crate) fn resolve_path_variable(value: OsString) -> Option<OsString> {
    std::path::absolute(value).ok().map(PathBuf::into_os_string)
}

/// `list` (a PATH-style list) with each relative directory made absolute, or `None` when one cannot be.
///
/// A worker runs in the state dir, so a relative PATH entry would lead somewhere else there. What an empty entry means
/// differs by platform and variable, so `empty_is_current` selects the search rule:
/// - Unix searches the current folder for an empty entry, so it becomes the client's folder (as an absolute path).
/// - Windows has no such rule: `Command` skips an empty entry and never searches the current folder, and a project
///   file must not stand in for the worker. The entry stays empty; made absolute it would become a searched folder.
fn absolute_path_list(list: &OsStr, empty_is_current: bool) -> Option<OsString> {
    let mut directories = Vec::new();
    for directory in std::env::split_paths(list) {
        let is_empty = directory.as_os_str().is_empty();
        if is_empty && !empty_is_current {
            directories.push(directory);
            continue;
        }
        let directory = if is_empty { PathBuf::from(".") } else { directory };
        directories.push(std::path::absolute(directory).ok()?);
    }
    std::env::join_paths(directories).ok()
}

/// The relative state directories, and, for a standalone binary, all environment paths and worker paths.
/// The service key and environment use the same values before its working directory changes.
pub(crate) fn path_variables() -> Vec<(&'static str, OsString)> {
    let mut variables: Vec<(&'static str, OsString)> = Vec::new();
    for (name, path) in relative_state_variables() {
        variables.push((name, path.into_os_string()));
    }
    if crate::is_standalone_binary() {
        for name in CLIENT_PATH_VARIABLES.into_iter().chain(USER_FOLDER_VARIABLES) {
            let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) else { continue };
            if let Some(value) = resolve_path_variable(value) {
                variables.push((name, value));
            }
        }
        // These are lists, so each directory is resolved, not the list as one path. Node ignores an empty NODE_PATH
        // entry on every platform, while PATH searches it as the current folder on Unix only.
        for (name, empty_is_current) in [("PATH", !cfg!(windows)), ("NODE_PATH", false)] {
            if let Some(list) = std::env::var_os(name).and_then(|list| absolute_path_list(&list, empty_is_current)) {
                variables.push((name, list));
            }
        }
        // Windows: the client resolves the worker once and forwards an absolute path or the lookup failure, so a copied
        // service never searches beside its own executable. Unix: a bare name stays bare, for native search against this PATH.
        #[cfg(feature = "bundles")]
        variables.extend(crate::project::model_assets::worker_variables());
    }
    variables
}

/// A variable name as the platform compares it: Windows environment names are case-insensitive, so two spellings must
/// collapse to one entry. Unix names stay as they are.
fn environment_name(name: OsString) -> OsString {
    if cfg!(windows) { name.to_ascii_uppercase() } else { name }
}

/// The full environment a standalone worker receives, which is also what its service is started with and what the
/// service key hashes. Using one value for all three keeps a direct run and a served run in agreement for any variable,
/// including ones only a custom worker reads, and a different environment selects a different service.
///
/// Only the variables ree knows to hold paths are resolved against the client's folder (the ones in `path_variables`).
/// Every other value is passed unchanged: ree cannot tell a path from any other text, so a relative path in an unknown
/// variable is read from the worker's working folder, the state dir, both directly and through a service.
pub(crate) fn worker_environment() -> Vec<(OsString, OsString)> {
    let mut variables: std::collections::BTreeMap<OsString, OsString> = std::env::vars_os().map(|(name, value)| (environment_name(name), value)).collect();
    for (name, value) in path_variables() {
        variables.insert(environment_name(name.into()), value);
    }
    // A flag-only service has its state from `--state-dir`, not from the environment: name it for the worker so the
    // service and a client that sets the same variable agree.
    let state = crate::state_dir(&crate::default_repo());
    variables.insert("RUR_STATE_DIR".into(), state.into_os_string());
    variables.into_iter().collect()
}

/// The engine options of a command line.
#[derive(Default, Debug, Clone)]
pub struct Args {
    pub offline: bool,
    pub roots: Vec<String>,
    pub project: Option<String>,
    pub place: Option<String>,
    pub viewport: Option<(f64, f64)>,
    pub scale: Option<f64>,
    pub mode: Option<String>,
    pub run_context: Option<String>,
    pub studio: Option<bool>,
    pub load_module: bool,
    pub load_string: bool,
    pub player: Option<String>,
    pub device: Option<String>,
    pub place_id: Option<f64>,
    pub streaming: Option<f64>,
    pub camera: Option<(f64, f64)>,
    pub rotation_clipping: Option<String>,
    pub snapshots: Vec<String>,
    pub snapshot_dir: Option<String>,
    pub no_snapshots: bool,
    pub verbose: bool,
    pub repo: Option<String>,
    pub mounts: Vec<project::Mount>,
    /// `--loopback`: one VM plays both sides of a client run (the fork's own; `--run-context loopback` is upstream's separate server and client engines)
    pub loopback: bool,
    pub datastore_dir: Option<String>,
    pub fresh_datastore: bool,
    /// `--rojo <file.project.json>`: a Rojo project file whose tree is mounted (read by [`prepare`])
    pub rojo: Option<String>,
    /// `--rojo-at Service/Name`: where a `--rojo` tree that is not a DataModel mounts
    pub rojo_at: Option<String>,
}

fn size(key: &str, text: &str) -> Result<(f64, f64), String> {
    let (w, h) = text.split_once(['x', 'X']).ok_or_else(|| format!("{key} needs WxH"))?;
    let number = |text: &str| text.parse::<f64>().map_err(|_| format!("{key}: not a number"));
    Ok((number(w)?, number(h)?))
}

impl Args {
    /// Takes `key` (with the argument after it, `value`) when it is an engine option: the number of
    /// arguments it used (1 for a flag, 2 for an option with a value), None for other keys.
    pub fn accept(&mut self, key: &str, value: Option<&String>) -> Result<Option<usize>, String> {
        let need = || value.cloned().ok_or_else(|| format!("{key} needs a value"));
        let number = |text: String| text.parse::<f64>().map_err(|_| format!("{key}: not a number"));
        match key {
            "--offline" => self.offline = true,
            "--verbose" => self.verbose = true,
            "--no-snapshots" => self.no_snapshots = true,
            "--loopback" => self.loopback = true,
            "--fresh-datastore" => self.fresh_datastore = true,
            "--datastore-dir" => self.datastore_dir = Some(need()?),
            "--load-module" => self.load_module = true,
            "--load-string" => self.load_string = true,
            "--roots" => self.roots = need()?.split(';').filter(|r| !r.is_empty()).map(str::to_string).collect(),
            "--project" => self.project = Some(need()?),
            "--place" => self.place = Some(need()?),
            "--viewport" => self.viewport = Some(size(key, &need()?)?),
            "--camera" => self.camera = Some(size(key, &need()?)?),
            "--scale" => self.scale = Some(number(need()?)?),
            "--mode" => self.mode = Some(need()?),
            "--run-context" => self.run_context = Some(need()?),
            "--studio" => {
                self.studio = Some(match need()?.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("--studio must be true or false".into()),
                })
            }
            "--player" => self.player = Some(need()?),
            "--device" => self.device = Some(need()?),
            "--place-id" => self.place_id = Some(number(need()?)?),
            "--streaming" => self.streaming = Some(number(need()?)?),
            "--rotation-clipping" => self.rotation_clipping = Some(need()?),
            "--snapshot" => self.snapshots.push(need()?),
            "--snapshot-dir" => self.snapshot_dir = Some(need()?),
            "--repo" => self.repo = Some(need()?),
            "--mount" => self.mounts.push(project::Mount::parse(&need()?, false)?),
            "--instance" => self.mounts.push(project::Mount::parse(&need()?, true)?),
            "--rojo" => self.rojo = Some(need()?),
            "--rojo-at" => self.rojo_at = Some(need()?),
            _ => return Ok(None),
        }
        Ok(Some(if matches!(key, "--offline" | "--verbose" | "--no-snapshots" | "--loopback" | "--fresh-datastore" | "--load-module" | "--load-string") { 1 } else { 2 }))
    }
}

/// `args` with every `--key=value` as `--key` `value` (the first `=` splits; values may hold more), so
/// both spellings parse alike: callers join values that start with "-" (a negative number) to their key.
pub fn split_equals(args: &[String]) -> Vec<String> {
    args.iter()
        .flat_map(|arg| match arg.strip_prefix("--").and_then(|rest| rest.split_once('=')) {
            Some((key, value)) if !key.is_empty() => vec![format!("--{key}"), value.to_string()],
            _ => vec![arg.clone()],
        })
        .collect()
}

/// The engine options of `args`; other arguments are skipped one at a time.
pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut result = Args::default();
    let mut index = 0;
    while index < args.len() {
        index += result.accept(&args[index], args.get(index + 1))?.unwrap_or(1);
    }
    Ok(result)
}

/// An absolute path as Node's path.resolve gives it (hosts pass resolved paths).
pub fn absolute(path: &str) -> String {
    let path = PathBuf::from(path);
    std::path::absolute(&path).unwrap_or(path).to_string_lossy().into_owned()
}

/// What a run needs from its arguments: the engine options, project roots, snapshot directory and
/// mounts.
pub struct Prepared {
    pub options: Options,
    pub roots: Vec<String>,
    pub snapshot_dir: Option<String>,
    /// --mount / --instance, relative paths resolved
    pub mounts: Vec<project::Mount>,
    /// `--rojo`'s project tree, mounted where the roots would mount ([`crate::mount::ProjectSpec::rojo`])
    pub rojo: Option<crate::rojo::Tree>,
}

impl Prepared {
    /// The folders the run's place is looked up by ([`crate::mount::place_roots`])
    pub fn place_roots(&self) -> Vec<String> {
        crate::mount::place_roots(&self.roots, self.rojo.as_ref())
    }
}

/// main.luau's option handling: engine options, roots and snapshots of `args`. `default_repo` is
/// the host's checkout when `--repo` is not given ([`Options::repo`]).
pub fn prepare(args: &Args, default_repo: &Path) -> Result<Prepared, String> {
    let repo = args.repo.as_ref().map(PathBuf::from).unwrap_or_else(|| default_repo.to_path_buf());
    // runs need state, so report a missing standalone cache before preparing the engine
    crate::try_state_dir(&repo)?;
    if args.rojo.is_none() && args.rojo_at.is_some() {
        return Err("--rojo-at places a Rojo project: it needs --rojo <file.project.json>".into());
    }
    if args.rojo.is_some() && !args.roots.is_empty() {
        return Err("--rojo and --roots both name the project's sources: pass one (a Rojo project file maps every folder it mounts)".into());
    }
    // The Rojo project file's tree, read here (a run prepared ahead notes the files read from here on: record/prepare.rs)
    let rojo = args.rojo.as_deref().map(|file| crate::rojo::Tree::read(Path::new(file), args.rojo_at.as_deref())).transpose()?;
    // Studio snapshots: the ones named (--snapshot-dir / --snapshot)
    let snapshot_dir = args.snapshot_dir.as_ref().map(|d| absolute(d));
    // Options::project: the project named, else the first root's folder, else here
    let project_dir = args.project.as_deref().map(|dir| PathBuf::from(absolute(dir)));
    // Without --roots there is no project: the run has its harness and mounts alone
    let roots: Vec<String> = args.roots.iter().map(|r| absolute(r)).collect();
    let project_dir = project_dir
        .or_else(|| rojo.as_ref().map(|tree| PathBuf::from(tree.folder())))
        .or_else(|| roots.first().and_then(|root| Path::new(root).parent()).map(Path::to_path_buf))
        .or_else(|| std::env::current_dir().ok());
    // (a Rojo project's folder stands for the place's root where a place is looked up)
    let place_roots = crate::mount::place_roots(&roots, rojo.as_ref());

    let (width, height) = args.viewport.unwrap_or((1920.0, 1080.0));
    let place_id = args
        .place_id
        .or_else(|| std::env::var("RUR_PLACE_ID").ok().and_then(|v| v.parse().ok()))
        .or_else(|| project::project_setting(&place_roots, "placeIds").and_then(|v| v.as_f64()))
        .or_else(|| project::declared_place_id(&place_roots))
        .unwrap_or(0.0);
    // Camera.ViewportSize (--camera WxH / RUR_CAMERA): the screen the run sits in, which can exceed
    // the rendered area. Defaults to the viewport.
    let camera = args.camera.or_else(|| {
        let spec = std::env::var("RUR_CAMERA").ok()?;
        let (w, h) = spec.split_once(['x', 'X'])?;
        Some((w.parse().ok()?, h.parse().ok()?))
    });
    let rotation_clipping = args
        .rotation_clipping
        .clone()
        .or_else(|| std::env::var("RUR_ROTATION_CLIPPING").ok())
        .unwrap_or_else(|| "enabled".into());
    if rotation_clipping != "enabled" && rotation_clipping != "disabled" {
        return Err("rotation-clipping must be enabled or disabled".into());
    }
    let mode = args.mode.clone().unwrap_or_else(|| "edit".into());
    // the device the client emulates (UserInputService's inputs), and for a phone its prices too (cost.rs device_scale)
    let device = args.device.clone().or_else(|| std::env::var("REE_DEVICE").ok().filter(|v| !v.is_empty())).unwrap_or_else(|| "desktop".into());
    if !["desktop", "touch", "gamepad", "phone"].contains(&device.as_str()) {
        return Err(format!("--device must be desktop, touch, gamepad or phone, not {device}"));
    }
    let run_context = args.run_context.as_deref().map(RunContext::parse).transpose()?.unwrap_or_default();
    let place_prices = crate::cost::PlacePrices::default().with(std::env::var("REE_LOCALIZED_PLACES").ok().as_deref(), std::env::var("REE_MATCH_PLACES").ok().as_deref())?;
    // A Studio Play-mode server: no local player, and no edit-mode server (Studio's edit DataModel is
    // both sides at once)
    if run_context == RunContext::Edit && mode != "edit" {
        return Err("--run-context edit needs --mode edit".into());
    }
    // A server and its client (src/loopback): a play session's two sides
    if run_context == RunContext::Loopback && mode == "edit" {
        return Err("--run-context loopback needs --mode play".into());
    }
    if run_context == RunContext::Server {
        if mode == "edit" {
            return Err("--run-context server needs --mode play".into());
        }
        if args.player.is_some() {
            return Err("--player names the local player; a server has none (a harness adds its players)".into());
        }
    }
    // `--loopback` is the fork's one-VM play of both sides, so it needs a client run: a server run, a separate-engines
    // loopback run (`--run-context loopback`) and an edit run each play a side of their own
    if args.loopback && run_context != RunContext::Client {
        return Err("--loopback plays both sides in one client run; drop --run-context (or use --run-context loopback alone for separate server and client engines)".into());
    }
    let local_player_name = match run_context {
        RunContext::Server => None,
        // Studio's edit DataModel has no player (Players is empty and LocalPlayer nil for a plugin or the command bar
        // outside Team Create; not measured: a headless session has no edit DataModel); `--player` gives the run one all the same
        RunContext::Edit => args.player.clone(),
        RunContext::Client | RunContext::Loopback => Some(args.player.clone().or_else(|| std::env::var("RUR_PLAYER").ok()).unwrap_or_else(|| "Player".into())),
    };
    let options = Options {
        offline: args.offline,
        viewport_width: width,
        viewport_height: height,
        camera_width: camera.map(|c| c.0),
        camera_height: camera.map(|c| c.1),
        clips_descendants_supports_rotation: rotation_clipping == "enabled",
        pixel_scale: args.scale.unwrap_or(1.5),
        mode,
        run_context,
        studio: args.studio.unwrap_or(true),
        load_module: args.load_module,
        load_string: args.load_string,
        verbose: args.verbose || std::env::var("RUR_VERBOSE").as_deref() == Ok("1"),
        local_player_name,
        local_player_user_id: std::env::var("RUR_PLAYER_ID").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0),
        local_player_display_name: std::env::var("RUR_PLAYER_DISPLAY").ok(),
        place_id,
        // main.luau: PlaceName = --place (the CLI passes roots, so --place only names the place then)
        place_name: if args.roots.is_empty() { "Place".into() } else { args.place.clone().unwrap_or_else(|| "Place".into()) },
        repo: repo.clone(),
        project: project_dir,
        streaming_target_radius: args.streaming.or_else(|| project::project_setting(&place_roots, "streamingTargetRadius").and_then(|v| v.as_f64())),
        place_prices,
        loopback: args.loopback,
        clock_probe: None,
        device,
        datastore_dir: args.datastore_dir.as_ref().map(|dir| PathBuf::from(absolute(dir))),
        fresh_datastore: args.fresh_datastore,
        ..Options::default()
    };
    let mounts = args
        .mounts
        .iter()
        .map(|mount| match &mount.target {
            project::MountTarget::Path(path) => project::Mount { segments: mount.segments.clone(), target: project::MountTarget::Path(absolute(path)) },
            _ => mount.clone(),
        })
        .collect();
    Ok(Prepared { options, roots, snapshot_dir, mounts, rojo })
}

/// What a program running engine commands brings to the engines they start: its name (the prefix
/// of the status lines on stderr), its checkout (the default [`Options::repo`]) and how an engine
/// is made (with its layers and bundle source). [`Host::default`] is `ree`'s own: no layers.
#[derive(Clone)]
pub struct Host {
    pub name: &'static str,
    pub repo: PathBuf,
    pub new_engine: Rc<dyn Fn(Options) -> Result<Rc<Engine>, String>>,
}

impl Default for Host {
    fn default() -> Self {
        Self { name: "ree", repo: crate::default_repo(), new_engine: Rc::new(Engine::new) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty entry is the client's folder on Unix and stays empty on Windows, wherever it sits in the list.
    #[test]
    fn path_lists_resolve_relative_and_empty_entries() {
        let current = std::env::current_dir().unwrap();
        for entries in [["", "helpers", "tools"], ["helpers", "", "tools"], ["helpers", "tools", ""]] {
            let list = std::env::join_paths(entries).unwrap();
            let resolved = absolute_path_list(&list, !cfg!(windows)).unwrap();
            let actual: Vec<_> = std::env::split_paths(&resolved).collect();
            let expected: Vec<_> = entries
                .iter()
                .map(|entry| if cfg!(windows) && entry.is_empty() { PathBuf::new() } else { current.join(entry) })
                .collect();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn node_path_lists_keep_empty_entries_ignored_on_every_platform() {
        let list = std::env::join_paths(["", "modules", ""]).unwrap();
        let resolved = absolute_path_list(&list, false).unwrap();
        assert_eq!(std::env::split_paths(&resolved).collect::<Vec<_>>(), [PathBuf::new(), std::path::absolute("modules").unwrap(), PathBuf::new()]);
    }

    #[test]
    fn engine_options_are_taken_and_other_arguments_skipped() {
        let argv: Vec<String> = ["--story", "x.luau", "--verbose", "--viewport", "800x600", "--mount", "ReplicatedStorage/Lib=lib", "--mode", "play", "--headless"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = parse(&argv).unwrap();
        assert!(args.verbose);
        assert_eq!(args.viewport, Some((800.0, 600.0)));
        assert_eq!(args.mode.as_deref(), Some("play"));
        assert_eq!(args.mounts.len(), 1);
        assert!(parse(&["--scale".to_string()]).unwrap_err().contains("needs a value"));
        let prepared = prepare(&args, Path::new("C:/somewhere")).unwrap();
        assert_eq!((prepared.options.viewport_width, prepared.options.viewport_height), (800.0, 600.0));
        assert!(prepared.roots.is_empty(), "mounts without roots: no implicit project");
        assert!(matches!(&prepared.mounts[0].target, project::MountTarget::Path(path) if Path::new(path).is_absolute()));
    }

    #[test]
    fn a_server_run_context_has_no_local_player_and_plays() {
        let args = |words: &[&str]| parse(&words.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        let client = prepare(&args(&["--mount", "ReplicatedStorage/Lib=lib", "--mode", "play"]), Path::new("C:/somewhere")).unwrap();
        assert_eq!((client.options.run_context, client.options.local_player_name.is_some()), (RunContext::Client, true));
        let server = prepare(&args(&["--mount", "ReplicatedStorage/Lib=lib", "--mode", "play", "--run-context", "server"]), Path::new("C:/somewhere")).unwrap();
        assert!(server.options.server() && server.options.local_player_name.is_none() && server.options.studio);
        let live = prepare(&args(&["--mode", "play", "--run-context", "server", "--studio", "false"]), Path::new("C:/somewhere")).unwrap();
        assert!(live.options.server() && !live.options.studio);
        assert!(parse(&["--studio".to_string(), "no".to_string()]).unwrap_err().contains("true or false"));
        let refused = |words: &[&str]| prepare(&args(words), Path::new("C:/somewhere")).err().unwrap_or_default();
        assert!(refused(&["--run-context", "server"]).contains("needs --mode play"));
        assert!(refused(&["--run-context", "server", "--mode", "play", "--player", "Someone"]).contains("a server has none"));
        assert!(refused(&["--run-context", "both", "--mode", "play"]).contains("client, server, edit or loopback"));
        assert!(refused(&["--run-context", "loopback"]).contains("needs --mode play"));
        let pair = prepare(&args(&["--mode", "play", "--run-context", "loopback", "--player", "Someone"]), Path::new("C:/somewhere")).unwrap();
        assert!(pair.options.run_context == RunContext::Loopback && !pair.options.server());
        assert_eq!(pair.options.local_player_name.as_deref(), Some("Someone"));
        let edit = prepare(&args(&["--mount", "ReplicatedStorage/Lib=lib", "--run-context", "edit"]), Path::new("C:/somewhere")).unwrap();
        assert!(edit.options.run_context == RunContext::Edit && !edit.options.server() && edit.options.local_player_name.is_none());
        let edit_player = prepare(&args(&["--run-context", "edit", "--player", "Someone"]), Path::new("C:/somewhere")).unwrap();
        assert_eq!(edit_player.options.local_player_name.as_deref(), Some("Someone"));
        assert!(refused(&["--run-context", "edit", "--mode", "play"]).contains("needs --mode edit"));
    }

    #[test]
    fn content_paths_are_read_from_the_project() {
        let args = |words: &[String]| parse(words).unwrap();
        let base = std::env::temp_dir().join("ree-launch-project");
        let (game, repo) = (base.join("game"), base.join("tools").join("ree"));
        let text = |path: &Path| path.to_string_lossy().into_owned();
        let project = |words: &[String]| prepare(&args(words), &repo).unwrap().options.project;
        let named = [String::from("--project"), text(&game), "--mount".into(), "ReplicatedStorage/Lib=lib".into()];
        assert_eq!(project(&named), Some(std::path::absolute(&game).unwrap()), "the project named");
        assert_eq!(project(&[]), std::env::current_dir().ok(), "no roots and no mounts: here");
        let roots = format!("{};{}", text(&game.join("shared")), text(&base.join("other").join("src")));
        assert_eq!(project(&["--roots".into(), roots]), Some(std::path::absolute(&game).unwrap()), "the first root's folder");
        assert_eq!(project(&["--mount".into(), "ReplicatedStorage/Lib=lib".into()]), std::env::current_dir().ok(), "mounts alone: here");
    }

    /// `--rojo <file>`: the project file's tree, read into the mounts the roots' place takes, ahead of the command line's
    /// own; no roots (nor an implicit project); the project's folder is the run's (unless `--project` names one) and the
    /// place's root (rur.json's place id for it). Refused: with `--roots`, `--rojo-at` alone, a file that isn't there.
    #[test]
    fn a_rojo_project_file_is_the_runs_project() {
        let base = std::env::temp_dir().join(format!("ree-launch-rojo-{}", std::process::id()));
        let game = base.join("game");
        std::fs::create_dir_all(game.join("src").join("shared")).unwrap();
        std::fs::write(game.join("default.project.json"), r#"{"name":"g","tree":{"$className":"DataModel","ReplicatedStorage":{"Shared":{"$path":"src/shared"},"Gone":{"$path":"gone"}}}}"#).unwrap();
        std::fs::write(base.join("rur.json"), r#"{"placeIds":{"game":42}}"#).unwrap();
        let words = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let file = game.join("default.project.json").to_string_lossy().into_owned();
        let prepare_words = |list: &[&str]| prepare(&parse(&split_equals(&words(list))).unwrap(), Path::new("C:/somewhere"));
        for spelled in [&["--rojo", &file, "--mount", "Workspace/X=x"][..], &[&format!("--rojo={file}"), "--mount", "Workspace/X=x"]] {
            let prepared = prepare_words(spelled).unwrap();
            assert!(prepared.roots.is_empty(), "no roots and no implicit project");
            let tree = prepared.rojo.as_ref().unwrap();
            assert_eq!(tree.mounts.len(), 1);
            assert_eq!(tree.mounts[0].segments, ["ReplicatedStorage", "Shared"]);
            assert!(matches!(&tree.mounts[0].target, project::MountTarget::Path(path) if Path::new(path) == game.join("src").join("shared")));
            assert_eq!(tree.missing, ["default.project.json maps ReplicatedStorage.Gone to gone, which doesn't exist"]);
            assert_eq!(prepared.mounts.len(), 1, "the command line's mounts, after the tree");
            assert_eq!(prepared.options.project.as_deref(), Some(game.as_path()), "the project file's folder");
            assert_eq!(prepared.place_roots(), [tree.folder()]);
            if std::env::var_os("RUR_PLACE_ID").is_none() {
                assert_eq!(prepared.options.place_id, 42.0, "the place's root is the project file's folder");
            }
        }
        let named = prepare_words(&["--rojo", &file, "--project", &base.to_string_lossy()]).unwrap();
        assert_eq!(named.options.project.as_deref(), Some(std::path::absolute(&base).unwrap().as_path()), "--project names another folder");
        let refused = |list: &[&str]| prepare_words(list).err().unwrap_or_default();
        assert!(refused(&["--rojo", &file, "--roots", &game.to_string_lossy()]).contains("--rojo and --roots"));
        assert!(refused(&["--rojo-at", "ServerStorage/Game"]).contains("needs --rojo"));
        assert!(refused(&["--rojo", &game.join("none.project.json").to_string_lossy()]).contains("no such file"));
        // (an empty --roots is no roots: the two go together)
        assert!(prepare_words(&["--rojo", &file, "--roots", ""]).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn key_equals_value_is_two_arguments() {
        let argv: Vec<String> = ["--place-id=-5", "--mount=ReplicatedStorage/Lib=lib", "--verbose", "x=y", "--=z"].iter().map(|s| s.to_string()).collect();
        assert_eq!(split_equals(&argv), ["--place-id", "-5", "--mount", "ReplicatedStorage/Lib=lib", "--verbose", "x=y", "--=z"]);
        let args = parse(&split_equals(&argv)).unwrap();
        assert_eq!(args.place_id, Some(-5.0));
        assert_eq!(args.mounts[0].segments, ["ReplicatedStorage", "Lib"]);
    }

    /// A library host never calls `init_binary`, so even an empty repo is not standalone and its cache is relative
    #[test]
    fn an_empty_host_repo_keeps_checkout_defaults() {
        // Either override wins over the host default checked below, so a caller that sets one would fail this test.
        // Rerun it in a child without them rather than removing them here, which would change the other tests' environment.
        let overrides = ["RUR_STATE_DIR", "RUR_CACHE_DIR"];
        if overrides.iter().any(|name| std::env::var_os(name).is_some()) {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "launch::tests::an_empty_host_repo_keeps_checkout_defaults", "--nocapture"]);
            for name in overrides {
                child.env_remove(name);
            }
            let output = child.output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        let prepared = prepare(&Args::default(), Path::new("")).unwrap();
        assert!(!crate::is_standalone(&prepared.options.repo));
        assert_eq!(crate::state_dir(&prepared.options.repo), PathBuf::from(".cache"));
    }
}

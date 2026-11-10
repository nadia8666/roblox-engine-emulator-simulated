//! Where an Animation's keyframes come from. A host turns loading on ([`enable`]; `ree record` /
//! `ree run` do); without it every track plays without keyframes, as before tracks ran (rur's
//! golden runs, a WebAssembly host). An AnimationId resolves, first match wins, to:
//!
//! 1. a sequence registered in this engine (KeyframeSequenceProvider:RegisterKeyframeSequence);
//! 2. `<id>.rbxm` / `<id>.rbxmx` in a mounted directory ([`Loading::directories`], `--animations`);
//! 3. the download cache, `<state dir>/animations/<id>.rbxm`;
//! 4. a download through the asset worker (its `asset` command, [`crate::project::model_assets::AssetWorker`],
//!    which handles any authentication), written into that cache (features `bundles`; not `--offline`
//!    beyond what the worker's own cache holds).
//!
//! Decoded clips are kept per process by file and stamp (a warm `ree serve` decodes each once).
//! An id that resolves nowhere is warned about once per engine and its tracks have Length 0.
//!
//! KeyframeSequenceProvider:GetKeyframeSequenceAsync resolves the same way ([`sequence`]): a clone
//! of the registered sequence, else the file's KeyframeSequence made as new instances (its plan
//! kept per process like the clips).

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use rbx_dom_weak::WeakDom;
use rustc_hash::FxHashMap;

use super::clip::{self, Clip, EngineTree, FileTree};
use crate::{Engine, InstanceId, files, project::ModelPlan};

/// How an engine loads keyframes ([`enable`]).
#[derive(Clone, Debug, Default)]
pub struct Loading {
    /// Directories of pre-fetched animations, `<id>.rbxm` / `<id>.rbxmx` (searched in order)
    pub directories: Vec<PathBuf>,
    /// Download what the directories and the cache lack (through the asset worker)
    pub download: bool,
}

#[derive(Default)]
pub(crate) struct Loader {
    loading: Option<Loading>,
    /// RegisterKeyframeSequence's ids
    pub registered: FxHashMap<String, InstanceId>,
    pub next_registered: u64,
    /// When each animation asset asked for arrives (by asset id)
    fetches: FxHashMap<String, f64>,
    /// Ids already warned about
    warned: Vec<String>,
    #[cfg(feature = "bundles")]
    worker: Option<crate::project::model_assets::AssetWorker>,
}

pub(crate) fn loader(engine: &Engine) -> &RefCell<Loader> {
    &super::extension(engine).loader
}

/// Turns keyframe loading on for `engine` (before scripts load animations).
pub fn enable(engine: &Engine, loading: Loading) {
    loader(engine).borrow_mut().loading = Some(loading);
}

/// What was made of model files, per process, by file and stamp.
type Kept<T> = Mutex<HashMap<PathBuf, (files::Stamp, Result<Arc<T>, String>)>>;

/// `make`'s result for model file `path`, made once per process while the file is unchanged.
fn kept<T>(cache: &Kept<T>, path: &Path, make: impl FnOnce(&WeakDom) -> Result<T, String>) -> Result<Arc<T>, String> {
    files::note(path);
    let stamp = files::stamp(path).ok_or_else(|| format!("{}: not found", path.display()))?;
    if let Some((kept, made)) = cache.lock().unwrap().get(path) {
        if *kept == stamp {
            return made.clone();
        }
    }
    let made = std::fs::read(path)
        .map_err(|error| format!("{}: {error}", path.display()))
        .and_then(|bytes| crate::project::decode_model(&bytes))
        .and_then(|dom| make(&dom))
        .map(Arc::new);
    cache.lock().unwrap().insert(path.to_path_buf(), (stamp, made.clone()));
    made
}

/// The clip in model file `path` (decoded once per process while the file is unchanged).
pub fn clip_from_file(path: &Path) -> Result<Arc<Clip>, String> {
    static CLIPS: std::sync::OnceLock<Kept<Clip>> = std::sync::OnceLock::new();
    kept(CLIPS.get_or_init(Default::default), path, |dom| Clip::find(&FileTree(dom), dom.root_ref()))
}

/// The KeyframeSequence in model file `path` as a plan to make it of (converted once per process
/// while the file is unchanged).
fn sequence_from_file(engine: &Engine, path: &Path) -> Result<Arc<ModelPlan>, String> {
    static SEQUENCES: std::sync::OnceLock<Kept<ModelPlan>> = std::sync::OnceLock::new();
    kept(SEQUENCES.get_or_init(Default::default), path, |dom| {
        let node = clip::sequence_node(&FileTree(dom), dom.root_ref())?;
        Ok(ModelPlan::new(&engine.reflection, dom, &[node]))
    })
}

/// The asset id of a content id (`rbxassetid://N`, `http(s)://www.roblox.com/asset/?id=N`, `N`).
pub fn asset_id(uri: &str) -> Option<u64> {
    let uri = uri.trim();
    let digits = if let Some(rest) = uri.strip_prefix("rbxassetid://") {
        rest
    } else if let Some(index) = uri.to_ascii_lowercase().find("id=") {
        uri[index + 3..].split('&').next().unwrap_or("")
    } else {
        uri
    };
    digits.trim().parse::<u64>().ok()
}

/// Where an AnimationId's keyframes are.
enum Found {
    /// Nothing to load: an empty id, or loading is off
    Nothing,
    /// A RegisterKeyframeSequence sequence
    Registered(InstanceId),
    /// A model file (downloaded when it had to be)
    File(PathBuf),
}

/// Where the AnimationId `uri` resolves (the order above); Err when it could not load. (The files it
/// looks for are noted: files::note.)
fn locate(engine: &Engine, uri: &str) -> Result<Found, String> {
    let found = locate_unnoted(engine, uri);
    if found.is_err() {
        files::note_unchecked("an animation failed to load");
    }
    found
}

fn locate_unnoted(engine: &Engine, uri: &str) -> Result<Found, String> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Ok(Found::Nothing);
    }
    let slot = loader(engine);
    let registered = slot.borrow().registered.get(uri).copied();
    if let Some(sequence) = registered {
        if engine.state.borrow().dom.get(sequence).is_none() {
            return Err(format!("{uri}: its registered KeyframeSequence was destroyed"));
        }
        return Ok(Found::Registered(sequence));
    }
    let Some(loading) = slot.borrow().loading.clone() else { return Ok(Found::Nothing) };
    let Some(id) = asset_id(uri) else { return Err(format!("{uri}: not an asset id")) };
    for directory in &loading.directories {
        for extension in ["rbxm", "rbxmx"] {
            let file = directory.join(format!("{id}.{extension}"));
            files::note(&file);
            if files::is_file(&file) {
                return Ok(Found::File(file));
            }
        }
    }
    let cache = crate::state_dir(&engine.options.repo).join("animations");
    let cached = cache.join(format!("{id}.rbxm"));
    files::note(&cached);
    if files::is_file(&cached) {
        return Ok(Found::File(cached));
    }
    if !loading.download {
        return Err(format!("{uri}: not in {} or the --animations directories", cache.display()));
    }
    download(engine, slot, uri, &cache, &cached)?;
    Ok(Found::File(cached))
}

/// What the AnimationId `uri` plays: Ok(None) when there is nothing to load (an empty id, or loading
/// is off), Err when it could not load.
pub(crate) fn resolve(engine: &Engine, uri: &str) -> Result<Option<Arc<Clip>>, String> {
    match locate(engine, uri)? {
        Found::Nothing => Ok(None),
        Found::Registered(sequence) => {
            let state = engine.state.borrow();
            let tree = EngineTree { state: &state, reflection: &engine.reflection };
            Ok(Some(Arc::new(Clip::from_sequence(&tree, sequence))))
        }
        Found::File(file) => clip_from_file(&file).map(Some),
    }
}

/// A new, unparented copy of the KeyframeSequence the AnimationId `uri` names
/// (KeyframeSequenceProvider:GetKeyframeSequenceAsync): the registered sequence cloned, else the
/// file's sequence made as instances. Err when it could not load or there is nothing to load.
pub fn sequence(engine: &Engine, uri: &str) -> Result<InstanceId, String> {
    match locate(engine, uri)? {
        Found::Nothing if uri.trim().is_empty() => Err("no animation id".into()),
        Found::Nothing => Err(format!("{uri}: this engine loads no animation files")),
        Found::Registered(sequence) => {
            crate::lua::instance::clone(engine, sequence).ok_or_else(|| format!("{uri}: its registered KeyframeSequence is not Archivable"))
        }
        Found::File(file) => {
            let plan = sequence_from_file(engine, &file)?;
            plan.instantiate(engine, None).first().copied().ok_or_else(|| format!("{}: the asset holds no KeyframeSequence", file.display()))
        }
    }
}

/// When the animation asset `uri` arrives in this engine: its first request starts its fetch
/// ([`crate::cost::ANIMATION_FETCH`]); later requests share it (an id's forms are one asset).
pub(crate) fn arrival(engine: &Engine, uri: &str) -> f64 {
    let key = asset_id(uri).map_or_else(|| uri.trim().to_string(), |id| id.to_string());
    let now = engine.state.borrow().sched.now;
    *loader(engine).borrow_mut().fetches.entry(key).or_insert(now + crate::cost::ANIMATION_FETCH)
}

/// Whether the AnimationId `uri` names an asset to fetch: a file found (a registered sequence is the
/// engine's own; nothing found loads nothing).
pub(crate) fn fetched_from_file(engine: &Engine, uri: &str) -> bool {
    !uri.trim().is_empty() && !loader(engine).borrow().registered.contains_key(uri.trim()) && matches!(locate_unnoted(engine, uri), Ok(Found::File(_)))
}

/// Downloads asset `uri` into `cached` the way keyframes download (the same switch and worker): what
/// other loaders fetch, such as an EditableImage's image (crate::images).
pub(crate) fn fetch(engine: &Engine, uri: &str, cache: &Path, cached: &Path) -> Result<(), String> {
    let slot = loader(engine);
    if !slot.borrow().loading.as_ref().is_some_and(|loading| loading.download) {
        return Err(format!("{uri}: this run downloads no assets"));
    }
    download(engine, slot, uri, cache, cached)
}

#[cfg(feature = "bundles")]
fn download(engine: &Engine, slot: &RefCell<Loader>, uri: &str, cache: &Path, cached: &Path) -> Result<(), String> {
    // Standalone only: the worker runs in the state dir and is sent an absolute path, so compare with its reply and copy
    // to that same path. A host's `cached` stays as it passed it, as before standalone binaries.
    let absolute_cached = if crate::is_standalone(&engine.options.repo) { Some(crate::project::model_assets::absolute_path(cached)?) } else { None };
    let cached = absolute_cached.as_deref().unwrap_or(cached);
    std::fs::create_dir_all(cache).map_err(|error| format!("{}: {error}", cache.display()))?;
    let mut loader = slot.borrow_mut();
    let worker = loader.worker.get_or_insert_with(|| crate::project::model_assets::AssetWorker::new(&engine.options.repo, engine.options.offline));
    let file = crate::timing::time("animations: download", || worker.fetch_asset(uri, cached))?;
    // A standalone worker can name the file it wrote by a link or another spelling of the same path (a state dir behind a
    // link, say). Copying a file onto itself would truncate it, so compare identity, not only the path text.
    let is_standalone = absolute_cached.is_some();
    let worker_wrote_it_there = file == cached || (is_standalone && crate::files::same_file(&file, cached));
    if !worker_wrote_it_there {
        std::fs::copy(&file, cached).map_err(|error| format!("{}: {error}", cached.display()))?;
    }
    Ok(())
}

#[cfg(not(feature = "bundles"))]
fn download(_: &Engine, _: &RefCell<Loader>, uri: &str, _: &Path, _: &Path) -> Result<(), String> {
    Err(format!("{uri}: this build downloads no assets"))
}

/// Whether `uri`'s failure is new to this engine (warn once).
pub(crate) fn first_failure(engine: &Engine, uri: &str) -> bool {
    let mut loader = loader(engine).borrow_mut();
    if loader.warned.iter().any(|seen| seen == uri) {
        return false;
    }
    loader.warned.push(uri.to_string());
    true
}

/// Stops the download worker (a process that exits next: record::run::release).
pub fn stop(engine: &Engine) {
    #[cfg(feature = "bundles")]
    if let Some(found) = engine.extension::<super::AnimationExtension>() {
        found.loader.borrow_mut().worker = None;
    }
    #[cfg(not(feature = "bundles"))]
    let _ = engine;
}

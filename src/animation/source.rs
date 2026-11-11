//! Where an Animation's keyframes come from. A host turns loading on ([`enable`]; `ree record` /
//! `ree run` do); without it every track plays without keyframes, as before tracks ran (rur's
//! golden runs, a WebAssembly host). An AnimationId resolves, first match wins, to:
//!
//! 1. a clip registered in this engine (KeyframeSequenceProvider:RegisterKeyframeSequence,
//!    AnimationClipProvider:RegisterAnimationClip and their Active forms);
//! 2. `<id>.rbxm` / `<id>.rbxmx` in a mounted directory ([`Loading::directories`], `--animations`);
//! 3. the download cache, `<state dir>/animations/<id>.rbxm`;
//! 4. a download through the asset worker (its `asset` command, [`crate::project::model_assets::AssetWorker`],
//!    which handles any authentication), written into that cache (features `bundles`; not `--offline`
//!    beyond what the worker's own cache holds).
//!
//! Decoded clips are kept per process by file and stamp (a warm `ree serve` decodes each once).
//! An id that resolves nowhere is warned about once per engine and its tracks have Length 0.
//!
//! The file's clip is a KeyframeSequence or a CurveAnimation ([`super::curve_clip`]).
//!
//! KeyframeSequenceProvider:GetKeyframeSequenceAsync resolves the same way ([`sequence`]): a clone
//! of the registered sequence, else the file's KeyframeSequence made as new instances (its plan
//! kept per process like the clips); of a CurveAnimation, the KeyframeSequence made of it.
//! AnimationClipProvider:GetAnimationClipAsync ([`animation_clip`]) gives the clip as it is.

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
    /// The ids of clips registered in this engine (RegisterKeyframeSequence, RegisterAnimationClip and their Active forms)
    pub registered: FxHashMap<String, InstanceId>,
    pub next_registered: u64,
    /// What each id that is not Active plays: the clip as it was when registered, None for an instance that is no clip
    snapshots: FxHashMap<String, Option<Arc<Clip>>>,
    /// The id of each content registered ([`digest`])
    digests: FxHashMap<u128, String>,
    /// The next `active://N`
    pub next_active: u64,
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
/// while the file is unchanged); of a CurveAnimation, the sequence made of it
/// ([`super::curve_clip::sequence`]).
fn sequence_from_file(engine: &Engine, path: &Path) -> Result<Arc<ModelPlan>, String> {
    static SEQUENCES: std::sync::OnceLock<Kept<ModelPlan>> = std::sync::OnceLock::new();
    kept(SEQUENCES.get_or_init(Default::default), path, |dom| {
        let (node, curve) = clip::clip_node(&FileTree(dom), dom.root_ref())?;
        if curve {
            let (made, sequence) = super::curve_clip::sequence(&FileTree(dom), node);
            return Ok(ModelPlan::new(&engine.reflection, &made, &[sequence]));
        }
        Ok(ModelPlan::new(&engine.reflection, dom, &[node]))
    })
}

/// The clip in model file `path`, a KeyframeSequence or a CurveAnimation, as a plan to make it of.
fn animation_clip_from_file(engine: &Engine, path: &Path) -> Result<Arc<ModelPlan>, String> {
    static CLIPS: std::sync::OnceLock<Kept<ModelPlan>> = std::sync::OnceLock::new();
    kept(CLIPS.get_or_init(Default::default), path, |dom| {
        let (node, _) = clip::clip_node(&FileTree(dom), dom.root_ref())?;
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
    /// A clip registered in this engine
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

/// What stands for the content of `root` and its descendants: their classes, names, stored properties, attributes and
/// tags, in order. Two instances built the same way have the same; a property that holds an instance counts as set,
/// whichever instance it is.
fn digest(state: &crate::State, reflection: &crate::Reflection, root: InstanceId) -> u128 {
    use std::{fmt::Write, hash::Hasher};
    let (mut first, mut second) = (std::collections::hash_map::DefaultHasher::new(), rustc_hash::FxHasher::default());
    let mut text = String::new();
    let mut stack = vec![(root, 0u32)];
    while let Some((id, depth)) = stack.pop() {
        let instance = state.dom.at(id);
        text.clear();
        let _ = write!(text, "{depth}|{}|{:?}|", reflection.class(instance.class).name, instance.name);
        for entry in instance.props.iter() {
            let _ = match entry.1.as_instance() {
                Some(_) => write!(text, "{}=instance;", entry.0),
                None => write!(text, "{}={:?};", entry.0, entry.1),
            };
        }
        let _ = write!(text, "|{:?}|{:?}", instance.attributes, instance.tags);
        first.write(text.as_bytes());
        second.write(text.as_bytes());
        stack.extend(instance.children.iter().rev().map(|&child| (child, depth + 1)));
    }
    (first.finish() as u128) << 64 | second.finish() as u128
}

/// KeyframeSequenceProvider:RegisterKeyframeSequence and AnimationClipProvider:RegisterAnimationClip: the id of
/// `instance`, a CurveAnimation (`curve` Some(true)), a KeyframeSequence (Some(false)) or something that is no clip
/// (None; the first of the two takes one). Measured in Studio 0.747: the id stands for the content. Registering the
/// same clip again, a clone of it or another built the same way gives the same id, and a clip changed in any way (a
/// key, its name, an attribute) gives another; a track of the id plays the clip as it was when registered, whatever
/// became of the instance since. Studio's id is 32 hexadecimal digits of that content; here the ids count up, one
/// per content. The id of what is no clip plays nothing (a track of Length 0 that stays at 0 s).
pub(crate) fn register(engine: &Engine, instance: InstanceId, curve: Option<bool>) -> String {
    let slot = loader(engine);
    let state = engine.state.borrow();
    let content = digest(&state, &engine.reflection, instance);
    if let Some(uri) = slot.borrow().digests.get(&content) {
        return uri.clone();
    }
    let clip = curve.map(|curve| Arc::new(Clip::of(&EngineTree { state: &state, reflection: &engine.reflection }, instance, curve)));
    drop(state);
    let uri = {
        let mut loader = slot.borrow_mut();
        loader.next_registered += 1;
        let uri = format!("rbxtemp://{}", loader.next_registered);
        loader.registered.insert(uri.clone(), instance);
        loader.snapshots.insert(uri.clone(), clip);
        loader.digests.insert(content, uri.clone());
        uri
    };
    engine.state.borrow_mut().dom.pin(instance);
    uri
}

/// Whether `uri` is the id of a registered instance that is no clip.
fn no_clip(engine: &Engine, uri: &str) -> bool {
    matches!(loader(engine).borrow().snapshots.get(uri.trim()), Some(None))
}

/// GetAnimationClipAsync's error for an id that never answers ([`animation_clip`]).
pub const NEVER: &str = "never";

/// What the AnimationId `uri` plays: Ok(None) when there is nothing to load (an empty id, or loading
/// is off), Err when it could not load.
pub(crate) fn resolve(engine: &Engine, uri: &str) -> Result<Option<Arc<Clip>>, String> {
    if let Some(kept) = loader(engine).borrow().snapshots.get(uri.trim()) {
        return Ok(kept.clone());
    }
    match locate(engine, uri)? {
        Found::Nothing => Ok(None),
        Found::Registered(registered) => {
            let state = engine.state.borrow();
            let tree = EngineTree { state: &state, reflection: &engine.reflection };
            Ok(Some(Arc::new(Clip::of(&tree, registered, is_curve(engine, &state, registered)))))
        }
        Found::File(file) => clip_from_file(&file).map(Some),
    }
}

fn is_curve(engine: &Engine, state: &crate::State, clip: InstanceId) -> bool {
    engine.reflection.class(state.dom.at(clip).class).name.as_ref() == "CurveAnimation"
}

/// A new, unparented copy of the KeyframeSequence the AnimationId `uri` names
/// (KeyframeSequenceProvider:GetKeyframeSequenceAsync): the registered sequence cloned, else the
/// file's sequence made as instances. Err when it could not load or there is nothing to load.
pub fn sequence(engine: &Engine, uri: &str) -> Result<InstanceId, String> {
    // (measured: the id of an instance that is no clip gives a new KeyframeSequence without keyframes)
    if no_clip(engine, uri) {
        let dom = WeakDom::new(rbx_dom_weak::InstanceBuilder::new("KeyframeSequence"));
        let plan = ModelPlan::new(&engine.reflection, &dom, &[dom.root_ref()]);
        return plan.instantiate(engine, None).first().copied().ok_or_else(|| format!("{uri}: no KeyframeSequence could be made of it"));
    }
    match locate(engine, uri)? {
        Found::Nothing if uri.trim().is_empty() => Err("no animation id".into()),
        Found::Nothing => Err(format!("{uri}: this engine loads no animation files")),
        Found::Registered(registered) => {
            let made = {
                let state = engine.state.borrow();
                is_curve(engine, &state, registered).then(|| {
                    let tree = EngineTree { state: &state, reflection: &engine.reflection };
                    super::curve_clip::sequence(&tree, registered)
                })
            };
            match made {
                // (measured: an `active://` CurveAnimation is unknown to GetKeyframeSequenceAsync)
                Some(_) if uri.trim().starts_with("active://") => Err(format!("Unknown 'active' animation: '{}'", uri.trim())),
                Some((dom, sequence)) => {
                    let plan = ModelPlan::new(&engine.reflection, &dom, &[sequence]);
                    plan.instantiate(engine, None).first().copied().ok_or_else(|| format!("{uri}: no KeyframeSequence could be made of it"))
                }
                None => crate::lua::instance::clone(engine, registered).ok_or_else(|| format!("{uri}: its registered KeyframeSequence is not Archivable")),
            }
        }
        Found::File(file) => {
            let plan = sequence_from_file(engine, &file)?;
            plan.instantiate(engine, None).first().copied().ok_or_else(|| format!("{}: the asset holds no KeyframeSequence", file.display()))
        }
    }
}

/// The clip the AnimationId `uri` names (AnimationClipProvider:GetAnimationClipAsync): a new,
/// unparented copy of the registered clip or of the file's, a KeyframeSequence or a CurveAnimation
/// as it is; the registered instance itself for an `active://` id (measured in Studio 0.747). Err
/// when it could not load or there is nothing to load: [`NEVER`] for the id of a registered instance
/// that is no clip, which Studio never answers (the call was still waiting after 110 s).
pub fn animation_clip(engine: &Engine, uri: &str) -> Result<InstanceId, String> {
    if no_clip(engine, uri) {
        return Err(NEVER.into());
    }
    match locate(engine, uri)? {
        Found::Nothing if uri.trim().is_empty() => Err("no animation id".into()),
        Found::Nothing => Err(format!("{uri}: this engine loads no animation files")),
        Found::Registered(registered) if uri.trim().starts_with("active://") => Ok(registered),
        Found::Registered(registered) => {
            crate::lua::instance::clone(engine, registered).ok_or_else(|| format!("{uri}: its registered clip is not Archivable"))
        }
        Found::File(file) => {
            let plan = animation_clip_from_file(engine, &file)?;
            plan.instantiate(engine, None).first().copied().ok_or_else(|| format!("{}: the asset holds no animation clip", file.display()))
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

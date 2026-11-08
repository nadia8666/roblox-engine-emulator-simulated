//! Asset manifest bundle mounts (rur.json `modelAssets`, `ree record --assets`): a project's manifest
//! ([`MANIFEST`]) names its bundles (`["Models/Props"] = { AssetId = 1, Version = 2 }`), each an
//! uploaded model asset a game installs at that path. Only the manifest's
//! folders are mounted at startup, with a placeholder where each bundle goes; a bundle imports when
//! a script finds, tests or touches its placeholder or looks inside it, before script code measures
//! or clones it. Imported as a game's asset installer installs it (an uploaded bundle in place, a
//! loose one's members beside it):
//! the placeholder becomes the bundle's one instance in place (its class, properties, attributes,
//! tags and children; its name stays the path's last segment, and a script holding it holds the
//! instance), and a `_Loose` bundle's members go into its parent folder. The bundles come from a
//! [`BundleSource`]: an asset worker process ([`AssetWorker`]) unless the host sets another
//! ([`Engine::set_bundle_source`]); a source whose files hold only the children of a bundle's
//! instance ([`BundleShape::Members`]) mounts each bundle's children in a folder, as before. A
//! manifest line `["<key>"] = { AliasOf = "<path>" }` is a copy of the Model at that path, made when
//! a script asks for the alias's path by name ([`requested`]).
//!
//! The bundles a run imported are remembered ([`remember`]; `recent-<mount>.txt` beside the bundle
//! files): the next mount of the same bundles decodes those of them its source already holds on
//! other threads ([`Decoding`]) while the project mounts and scripts start, and imports take them
//! from there. A thread that runs many engines keeps bundles converted between them
//! ([`keep_bundles`]).
use super::plan::Mapping;
use super::*;
use serde_json::json;
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{Receiver, RecvTimeoutError, channel},
    },
    time::Duration,
};

/// Where manifest bundles come from.
pub trait BundleSource {
    /// The manifest's bundle keys (`Visuals/...`, `Models/...`) of the project folder `project`.
    fn catalog(&mut self, project: &Path) -> Result<Vec<String>, String>;
    /// A model file holding bundle `key`, as [`BundleSource::shape`] says.
    fn bundle(&mut self, project: &Path, key: &str) -> Result<PathBuf, String>;
    /// Bundle `key`'s model file when the source holds it already (no download, no worker): what
    /// [`BundleSource::bundle`] would return. None when it would have to fetch it (the default).
    fn cached(&mut self, _av: &Path, _key: &str) -> Option<PathBuf> {
        None
    }
    /// What the source's bundle files hold: [`BundleShape::Members`] unless it says otherwise.
    fn shape(&self) -> BundleShape {
        BundleShape::Members
    }
}

/// What a [`BundleSource`]'s bundle files hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BundleShape {
    /// The children of the bundle's one instance (an asset worker's `bundle` command, which
    /// roblox-ui-renderer's asset helper asks for): the instance's class and values are not in the
    /// file, so its path holds a Folder of them
    #[default]
    Members,
    /// The bundle as uploaded (the asset's own file): the instance the game installs at its path
    /// (`InsertService:LoadAssetVersion` wraps it; the installer takes the wrapper's one child)
    Uploaded,
}

/// The last segment of a loose bundle's key: its members go into the folder above
const LOOSE: &str = "_Loose";

/// Where a project folder keeps its asset manifest: a Luau module returning a table with one bundle a
/// line, `["<key>"] = { AssetId = <id>, Version = <version>, ... },`
pub const MANIFEST: &str = "asset-manifest.luau";

/// The warning of a bundle mount whose roots have no manifest
const NO_MANIFEST: &str = "modelAssets: no project folder above the roots holds the asset manifest";

fn is_loose(key: &str) -> bool {
    key.rsplit('/').next() == Some(LOOSE)
}

#[derive(Default)]
pub struct ModelAssets {
    source: Option<Box<dyn BundleSource>>,
    /// What the source's files hold
    shape: BundleShape,
    project: PathBuf,
    /// The bundles waiting at each placeholder (the bundle it stands for) or folder (loose ones)
    pending: FxHashMap<InstanceId, Vec<String>>,
    /// Bundles decoded ahead (the last run's imports)
    decoding: Option<Arc<Decoding>>,
    /// Where this mount's imports are remembered, and those of the last run
    recent: Option<(PathBuf, Vec<String>)>,
    /// Where the scene's imports are remembered ([`ModelAssetsOverride::scene`]), and those of its last
    /// run when it has one
    scene: Option<(PathBuf, Option<Vec<String>>)>,
    /// The keys imported so far, in order
    imported: Vec<String>,
    /// The instance the bundles' paths start at
    root: Option<InstanceId>,
    /// The manifest's aliases of this mount not asked for yet: each key with the path of the model it copies ([`aliases_in`],
    /// [`requested`])
    aliases: FxHashMap<String, String>,
}

fn folder(engine: &Engine, parent: InstanceId, name: &str) -> InstanceId {
    let mut state = engine.state.borrow_mut();
    // Materialize the Studio skeleton first so it cannot add duplicates after the real bundles.
    super::materialize(&mut state, &engine.reflection, parent);
    if let Some(child) = state.dom.find_child(parent, name.as_bytes()) {
        return child;
    }
    let class = engine.reflection.class_id("Folder").unwrap();
    let child = core::create_named(&mut state, &engine.reflection, class, name);
    state.dom.attach(child, parent);
    child
}

/// Mounts the manifest's bundle folders (rur.json `modelAssets`, or `chosen`'s mount and prefixes;
/// a `*` prefix selects every bundle).
pub fn mount(engine: &Engine, roots: &[String], chosen: &ModelAssetsOverride) {
    let config = project_setting(roots, "modelAssets").unwrap_or(serde_json::Value::Null);
    let Some(mount) = chosen.mount.as_deref().or(config["mount"].as_str()) else { return };
    let prefixes: Vec<String> = match &chosen.prefixes {
        Some(prefixes) => prefixes.clone(),
        None => config["prefixes"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect(),
    };
    let Some(project) = roots.iter().map(Path::new).filter_map(Path::parent)
        .find(|root| {
            crate::files::note(&root.join(MANIFEST));
            root.join(MANIFEST).is_file()
        }) else {
        engine.warn(NO_MANIFEST);
        return;
    };
    let mut source = engine.take_bundle_source().unwrap_or_else(|| Box::new(AssetWorker::new(&engine.options.repo, engine.options.offline)));
    let catalog = match crate::timing::time("bundles: catalog", || source.catalog(project)) {
        Ok(catalog) => catalog,
        Err(error) => { engine.warn(format!("modelAssets: {error}")); return; }
    };
    let mut segments = mount.split('.');
    let Some(service) = segments.next() else { return };
    let Ok(mut parent) = services::get_service(engine, engine.world().game, service) else { return };
    for name in segments { parent = folder(engine, parent, name); }
    let chosen_key = |key: &str| prefixes.iter().any(|prefix| prefix == "*" || key == prefix || key.strip_prefix(prefix.as_str()).is_some_and(|rest| rest.starts_with('/')));
    let mut aliases = aliases_in(project);
    let selected = catalog.iter().map(String::as_str).filter(|key| chosen_key(key) && !aliases.contains_key(*key));
    let shape = source.shape();
    let pending = place_all(engine, parent, selected, shape);
    aliases.retain(|key, _| chosen_key(key));
    let (recent, scene) = recent_lists(&engine.options.repo, project, mount, &prefixes, chosen.scene.as_deref());
    let reflection = engine.reflection.clone();
    let decoding = ahead(&recent, &scene).and_then(|keys| {
        let selected: FxHashSet<&str> = pending.values().flatten().map(String::as_str).collect();
        let files: Vec<(String, PathBuf)> = keys
            .iter()
            .filter(|key| selected.contains(key.as_str()))
            .filter_map(|key| source.cached(project, key).map(|path| (key.clone(), path)))
            .filter(|(_, path)| kept_plan(path).is_none())
            .collect();
        Decoding::start(files, reflection)
    });
    engine.state.borrow_mut().project.model_assets = ModelAssets { source: Some(source), shape, project: project.into(), pending, decoding, recent, scene, imported: Vec::new(), root: Some(parent), aliases };
}

/// The folders of bundles `keys` below `parent` (as a game's installer makes a path's folders), with
/// the bundles waiting at each: a bundle at the placeholder of its last segment (one
/// a [`BundleShape::Uploaded`] source fills becomes the bundle's instance: [`flags::BUNDLE_ROOT`]),
/// a loose bundle at the folder its members go into.
fn place_all<'a>(engine: &Engine, parent: InstanceId, keys: impl Iterator<Item = &'a str>, shape: BundleShape) -> FxHashMap<InstanceId, Vec<String>> {
    let mut pending: FxHashMap<InstanceId, Vec<String>> = FxHashMap::default();
    for key in keys {
        let mut target = parent;
        for name in key.split('/').filter(|name| *name != LOOSE) {
            target = folder(engine, target, name);
        }
        if shape == BundleShape::Uploaded && !is_loose(key) {
            engine.state.borrow_mut().dom.at_mut(target).set_flag(flags::BUNDLE_ROOT, true);
        }
        pending.entry(target).or_default().push(key.to_owned());
    }
    pending
}

/// `ree serve --warm`: the plans of the bundles the last recordings of this mount imported
/// (`recent-*.txt`, as [`mount`] chooses the mount and prefixes), read or converted into this
/// thread's kept store on the thread pool, so the next recording takes them from memory. Bundles
/// the source would have to fetch are left to the recordings. Returns how many of them are kept now and
/// how many were read for it (the rest were kept already).
pub fn warm(repo: &Path, roots: &[String], chosen: &ModelAssetsOverride, reflection: &Reflection) -> (usize, usize) {
    use rayon::prelude::*;
    let Some(store) = store() else { return (0, 0) };
    let config = project_setting(roots, "modelAssets").unwrap_or(serde_json::Value::Null);
    let Some(mount) = chosen.mount.as_deref().or(config["mount"].as_str()) else { return (0, 0) };
    let prefixes: Vec<String> = match &chosen.prefixes {
        Some(prefixes) => prefixes.clone(),
        None => config["prefixes"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect(),
    };
    let Some(project) = roots.iter().map(Path::new).filter_map(Path::parent).find(|root| root.join(MANIFEST).is_file()) else { return (0, 0) };
    let Some(recent) = recent_file(repo, project, mount, &prefixes, None) else { return (0, 0) };
    let keys: Vec<String> = std::fs::read_to_string(&recent).map(|text| text.lines().filter(|line| !line.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    let mut source = AssetWorker::new(repo, true);
    let cached: Vec<PathBuf> = keys.iter().filter_map(|key| source.cached(project, key)).collect();
    let files: Vec<&PathBuf> = cached.iter().filter(|path| kept_plan(path).is_none()).collect();
    let read = files
        .par_iter()
        .filter(|path| {
            let Ok(plan) = convert_file(reflection, path) else { return false };
            let Some(stamp) = file_stamp(path) else { return false };
            let plan = crate::alloc::long_lived(|| Arc::new(plan.compacted()));
            store.lock().is_ok_and(|mut kept| {
                crate::alloc::long_lived(|| kept.insert(path.to_path_buf(), (stamp, plan)));
                true
            })
        })
        .count();
    (cached.iter().filter(|path| kept_plan(path).is_some()).count(), read)
}

/// Before the children of `id` are looked at: the bundles waiting there import, the one `id` stands
/// for first (it becomes that bundle's instance), then the loose ones whose members go into it.
pub fn materialize(engine: &Engine, id: InstanceId) {
    import(engine, id, false);
}

/// Before a script finds, tests or touches `id`, a bundle's placeholder ([`flags::BUNDLE_ROOT`]): it
/// becomes the bundle's instance (class, properties, attributes, tags, children) in place, so no
/// script sees a Folder where the game has the bundle's instance, and one that holds it holds that
/// instance; loose bundles whose members go into it wait until a script looks inside it.
pub fn reveal(engine: &Engine, id: InstanceId) {
    import(engine, id, true);
}

fn import(engine: &Engine, id: InstanceId, own_only: bool) {
    let (keys, mut source, project, decoding, shape) = {
        let mut state = engine.state.borrow_mut();
        let state = &mut *state;
        let assets = &mut state.project.model_assets;
        let Some(pending) = assets.pending.get_mut(&id) else {
            if own_only {
                state.dom.at_mut(id).set_flag(flags::BUNDLE_ROOT, false);
            }
            return;
        };
        // (an import under way, whose events ran a script: this waits for the next look)
        if assets.source.is_none() {
            return;
        }
        let keys: Vec<String> = if own_only {
            let own = pending.iter().filter(|key| !is_loose(key)).cloned().collect();
            pending.retain(|key| is_loose(key));
            own
        } else {
            // (the bundle `id` stands for before the members other bundles put in it)
            let mut keys = std::mem::take(pending);
            keys.sort_by_key(|key| is_loose(key));
            keys
        };
        if pending.is_empty() {
            assets.pending.remove(&id);
        }
        state.dom.at_mut(id).set_flag(flags::BUNDLE_ROOT, false);
        if keys.is_empty() {
            return;
        }
        let source = assets.source.take().expect("checked");
        assets.imported.extend(keys.iter().cloned());
        (keys, source, assets.project.clone(), assets.decoding.clone(), assets.shape)
    };
    for key in keys {
        let ahead = crate::timing::time("bundles: decoded ahead", || decoding.as_ref().and_then(|decoding| decoding.take(&key)));
        let result = match ahead {
            // (a plan kept across runs is copied to this thread's long-lived heap: crate::alloc)
            Some((path, converted)) => converted.map(|plan| (path.clone(), keep(&path, Arc::new(if kept_plans() { crate::alloc::long_lived(|| plan.compacted()) } else { plan })))),
            None => crate::timing::time("bundles: fetch", || source.bundle(&project, &key)).and_then(|path| plan_for(engine, &path).map(|plan| (path, plan))),
        };
        match &result {
            Ok((path, _)) => crate::files::note(path),
            Err(error) => {
                crate::files::note_unchecked("a bundle failed to load");
                crate::output::diagnostic(&format!("ree: bundle {key} did not load: {error}"));
            }
        }
        match result.map(|(_, plan)| plan) {
            Ok(plan) => install(engine, id, &key, &plan, shape),
            Err(error) => engine.warn(format!("modelAssets {key}: {error}")),
        }
    }
    engine.state.borrow_mut().project.model_assets.source = Some(source);
}

/// How a bundle's plan goes in at the instance it waits at.
enum Installing {
    /// Its roots as children of the instance (a [`BundleShape::Members`] file)
    Roots,
    /// Its one root is the instance, the placeholder at its path (an uploaded bundle)
    As,
    /// The children of its one root as children of the instance (a loose bundle)
    Members,
}

/// Puts bundle `key`'s plan in at `id`, the instance it waits at, as a game's installer installs it.
fn install(engine: &Engine, id: InstanceId, key: &str, plan: &Arc<ModelPlan>, shape: BundleShape) {
    let how = match shape {
        BundleShape::Members => Installing::Roots,
        BundleShape::Uploaded if plan.root_count() != 1 => {
            engine.warn(format!(
                "modelAssets {key}: the bundle holds {} instances at its top, where the game installs a bundle of one; they are mounted in a folder at its path",
                plan.root_count()
            ));
            Installing::Roots
        }
        BundleShape::Uploaded if is_loose(key) => Installing::Members,
        BundleShape::Uploaded => Installing::As,
    };
    // Imported children replace only the Studio placeholders with the same names.
    let names: FxHashSet<&[u8]> = match how {
        Installing::Roots => plan.root_names().collect(),
        Installing::As | Installing::Members => plan.first_root_child_names().collect(),
    };
    let obsolete = {
        let state = engine.state.borrow();
        state.dom.at(id).children.iter().copied().filter(|child| {
            let child = state.dom.at(*child);
            child.has(flags::STUDIO_ONLY) && names.contains(child.name.as_bytes())
        }).collect::<Vec<_>>()
    };
    for child in obsolete {
        crate::lua::instance::set_parent(engine, child, None).ok();
    }
    for warning in plan.warnings() {
        engine.warn(warning.clone());
    }
    // (the roots now, the rest as scripts look at it; all of it in the world, which the
    // recorder and the world's queries read whole)
    crate::timing::time("bundles: instantiate", || {
        let mut state = engine.state.borrow_mut();
        let made = match how {
            Installing::Roots => plan.instantiate_lazy(&mut state, &engine.reflection, id),
            Installing::As => {
                plan.instantiate_lazy_as(&mut state, &engine.reflection, id);
                vec![id]
            }
            Installing::Members => plan.instantiate_lazy_members(&mut state, &engine.reflection, id),
        };
        let workspace = engine.world().workspace;
        if id == workspace || state.dom.is_descendant_of(id, workspace) {
            for instance in made {
                super::materialize_lazy_subtree(&mut state, &engine.reflection, instance);
            }
        }
    });
}

/// A child `name` of `parent` a script looked up by name (FindFirstChild, WaitForChild, an index) and did not find, below
/// the mount: when that path is a manifest alias's, the alias is made now, once, as the game's installer makes it when a
/// client asks for the path ([`install_alias`]); a folder on the way to an alias is made as the installer makes the
/// alias's folders. What is at the path then, if anything. An alias nothing asks for is never made, and a listing does not
/// make one.
pub fn requested(engine: &Engine, parent: InstanceId, name: &[u8]) -> Option<InstanceId> {
    let alias = {
        let mut state = engine.state.borrow_mut();
        let state = &mut *state;
        let assets = &mut state.project.model_assets;
        let root = assets.root.filter(|_| !assets.aliases.is_empty())?;
        // (a lookup that missed outside the mount allocates nothing)
        let mut at = parent;
        while at != root {
            at = state.dom.at(at).parent?;
        }
        let mut path = vec![String::from_utf8_lossy(name).into_owned()];
        let mut at = parent;
        while at != root {
            let node = state.dom.at(at);
            path.push(String::from_utf8_lossy(node.name.as_bytes()).into_owned());
            at = node.parent?;
        }
        path.reverse();
        let wanted = path.join("/");
        match assets.aliases.remove(&wanted) {
            Some(model) => Some((wanted, model)),
            None => {
                let below = format!("{wanted}/");
                assets.aliases.keys().any(|key| key.starts_with(&below)).then_some(())?;
                None
            }
        }
    };
    match alias {
        Some((key, model)) => install_alias(engine, &key, &model),
        None => {
            folder(engine, parent, &String::from_utf8_lossy(name));
        }
    }
    engine.state.borrow().dom.find_child(parent, name)
}

/// Alias `key` of the model at path `model`, as the game's installer makes one: a copy of that model (its bundle imported
/// first), named as the key's last segment, in the key's folders (made where missing); none when the key's path holds an
/// instance already.
fn install_alias(engine: &Engine, key: &str, model: &str) {
    let Some(root) = engine.state.borrow().project.model_assets.root else { return };
    let Some(found) = model.split('/').try_fold(root, |at, name| child_of(engine, at, name)) else {
        engine.warn(format!("modelAssets {key}: its AliasOf {model} is not among the bundles"));
        return;
    };
    let class = engine.state.borrow().dom.at(found).class;
    if !engine.reflection.class_id("Model").is_some_and(|wanted| engine.reflection.class(class).is_a(wanted)) {
        engine.warn(format!("modelAssets {key}: its AliasOf {model} is a {}, and an alias copies a Model", engine.reflection.class(class).name));
        return;
    }
    let names: Vec<&str> = key.split('/').collect();
    let (name, folders) = names.split_last().expect("a key has a segment");
    let parent = folders.iter().fold(root, |at, folder_name| child_of(engine, at, folder_name).unwrap_or_else(|| folder(engine, at, folder_name)));
    if child_of(engine, parent, name).is_some() {
        return;
    }
    super::materialize_deep(engine, found);
    let copy = {
        let mut state = engine.state.borrow_mut();
        let copy = core::clone_instance(&mut state, &engine.reflection, found);
        if let Some(copy) = copy {
            core::raw_set_silent(&mut state, &engine.reflection, copy, K::Name as NameId, Value::String(Str::from(*name)));
            state.dom.attach(copy, parent);
        }
        copy
    };
    if copy.is_none() {
        engine.warn(format!("modelAssets {key}: its AliasOf {model} is not Archivable"));
    }
}

/// The child `name` of `at` as a script finds it: what waits there imported first, and a bundle's placeholder made the
/// bundle's instance.
fn child_of(engine: &Engine, at: InstanceId, name: &str) -> Option<InstanceId> {
    crate::lua::instance::materialize(engine, at);
    let child = engine.state.borrow().dom.find_child(at, name.as_bytes())?;
    if engine.state.borrow().dom.at(child).has(flags::BUNDLE_ROOT) {
        reveal(engine, child);
    }
    Some(child)
}

/// Remembers the bundles this engine imported, for the next mount of the same bundles to decode
/// ahead (a guess: a bundle imported differently is fetched and decoded as usual): the mount's list
/// (what `ree serve --warm` and a scene's first run read) and the scene's, which an empty list
/// replaces too (a scene that imports nothing then decodes nothing ahead: a profile's scenes ran one
/// after another, and each decoded the last one's bundles, 470 MB in a lobby after the mesh effects).
pub fn remember(engine: &Engine) {
    let state = engine.state.borrow();
    let assets = &state.project.model_assets;
    let mut seen = FxHashSet::default();
    let keys: Vec<&str> = assets.imported.iter().map(String::as_str).filter(|key| seen.insert(*key)).collect();
    if let Some((path, previous)) = &assets.recent {
        if !keys.is_empty() && !keys.iter().copied().eq(previous.iter().map(String::as_str)) {
            let _ = crate::files::write_atomic(path, keys.join("\n").as_bytes());
        }
    }
    if let Some((path, previous)) = &assets.scene {
        if !previous.as_ref().is_some_and(|previous| keys.iter().copied().eq(previous.iter().map(String::as_str))) {
            let _ = crate::files::write_atomic(path, keys.join("\n").as_bytes());
        }
    }
}

/// Stops the engine's bundle source (the asset worker) ahead of a process exit that skips the
/// engine's teardown.
pub fn stop(engine: &Engine) {
    let source = engine.state.borrow_mut().project.model_assets.source.take();
    drop(source);
}

/// The mount's list of the bundles its last run imported and the scene's (each with where it is kept;
/// the scene's is None when it has not run).
#[allow(clippy::type_complexity)]
fn recent_lists(repo: &Path, project: &Path, mount: &str, prefixes: &[String], scene: Option<&str>) -> (Option<(PathBuf, Vec<String>)>, Option<(PathBuf, Option<Vec<String>>)>) {
    let read = |path: &Path| std::fs::read_to_string(path).ok().map(|text| text.lines().filter(|line| !line.is_empty()).map(str::to_string).collect::<Vec<String>>());
    let recent = recent_file(repo, project, mount, prefixes, None).map(|path| {
        let keys = read(&path).unwrap_or_default();
        (path, keys)
    });
    let scene = scene.and_then(|scene| recent_file(repo, project, mount, prefixes, Some(scene))).map(|path| {
        let keys = read(&path);
        (path, keys)
    });
    (recent, scene)
}

/// The bundles to decode ahead: the scene's last run's imports, or, on its first run, the mount's last run's.
fn ahead<'a>(recent: &'a Option<(PathBuf, Vec<String>)>, scene: &'a Option<(PathBuf, Option<Vec<String>>)>) -> Option<&'a Vec<String>> {
    match scene {
        Some((_, Some(keys))) => Some(keys),
        _ => recent.as_ref().map(|(_, keys)| keys),
    }
}

/// `<state dir>/model-bundles/recent-<digest of the project folder, mount and prefixes>.txt`, or with a scene
/// ([`ModelAssetsOverride::scene`]) `recent-<digest of those and the scene>.txt`
fn recent_file(repo: &Path, project: &Path, mount: &str, prefixes: &[String], scene: Option<&str>) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let directory = crate::state_dir(repo).join("model-bundles");
    std::fs::create_dir_all(&directory).ok()?;
    let name = match scene {
        Some(scene) => serde_json::to_string(&json!([project, mount, prefixes, scene])).ok()?,
        None => serde_json::to_string(&json!([project, mount, prefixes])).ok()?,
    };
    let digest: String = Sha256::digest(name.as_bytes()).iter().take(8).map(|byte| format!("{byte:02x}")).collect();
    Some(directory.join(format!("recent-{digest}.txt")))
}

type Stamp = crate::files::Stamp;

fn file_stamp(path: &Path) -> Option<Stamp> {
    crate::files::stamp(path)
}

/// Converted bundles kept between engines, by file (its size and modification time unchanged): one
/// store a service's workers share, so a bundle is converted once for all of them.
pub type BundleStore = Arc<Mutex<FxHashMap<PathBuf, (Stamp, Arc<ModelPlan>)>>>;

thread_local! {
    /// The store of a thread that runs many engines ([`keep_bundles`], [`keep_bundles_in`])
    static KEPT: RefCell<Option<BundleStore>> = const { RefCell::new(None) };
}

/// For a thread that runs many engines one after another (a `ree serve` worker): bundle files
/// stay converted between its engines ([`ModelPlan`]), so a bundle file is decoded and converted
/// once while it is unchanged, and each engine only makes its instances.
pub fn keep_bundles() {
    KEPT.with(|kept| {
        kept.borrow_mut().get_or_insert_with(|| crate::alloc::long_lived(BundleStore::default));
    });
}

/// [`keep_bundles`] with `store`, which other threads keep their bundles in too (a service's workers).
pub fn keep_bundles_in(store: BundleStore) {
    KEPT.with(|kept| *kept.borrow_mut() = Some(store));
}

fn store() -> Option<BundleStore> {
    KEPT.with(|kept| kept.borrow().clone())
}

/// Drops the kept plans of bundle files that changed or are gone (a manifest pinning a new version
/// replaces a bundle's file: the old version's plan would be kept for nothing while the service lives).
pub fn forget_stale_bundles() {
    if let Some(store) = store() {
        if let Ok(mut kept) = store.lock() {
            kept.retain(|path, (stamp, _)| file_stamp(path) == Some(*stamp));
        }
    }
}

fn kept_plan(path: &Path) -> Option<Arc<ModelPlan>> {
    let stamp = file_stamp(path)?;
    let store = store()?;
    let kept = store.lock().ok()?;
    let (kept_stamp, plan) = kept.get(path)?;
    (*kept_stamp == stamp).then(|| plan.clone())
}

/// A converted bundle file, kept when the thread keeps bundles.
fn keep(path: &Path, plan: Arc<ModelPlan>) -> Arc<ModelPlan> {
    if let (Some(stamp), Some(store)) = (file_stamp(path), store()) {
        if let Ok(mut kept) = store.lock() {
            crate::alloc::long_lived(|| kept.insert(path.to_path_buf(), (stamp, plan.clone())));
        }
    }
    plan
}

/// Whether this thread keeps converted bundles ([`keep_bundles`]).
fn kept_plans() -> bool {
    KEPT.with(|kept| kept.borrow().is_some())
}

/// A bundle file converted: kept, else read from its converted file, else read, decoded and
/// converted now.
fn plan_for(engine: &Engine, path: &Path) -> Result<Arc<ModelPlan>, String> {
    if let Some(plan) = kept_plan(path) {
        crate::profiler::caches::hit(crate::profiler::caches::Cache::BundlePlans);
        return Ok(plan);
    }
    let plan = crate::timing::time("bundles: convert", || convert_file(&engine.reflection, path))?;
    let plan = if kept_plans() { crate::alloc::long_lived(|| plan.compacted()) } else { plan };
    Ok(keep(path, Arc::new(plan)))
}

/// Bundle file `path` converted: read from the converted file beside it (`<file>.plan`) when this
/// build wrote it for the file as it is, else read, decoded and converted now, and written there
/// for the next run (a one-shot recording decoded and converted its ~100 bundles on 8 threads
/// each time: 2 s of CPU in a quarter of a second, and the decoded files' memory).
fn convert_file(reflection: &Reflection, path: &Path) -> Result<ModelPlan, String> {
    let cache = converted_path(path);
    let key = converted_key(path);
    if let Some(plan) = key.as_ref().and_then(|key| read_plan(&cache, key)) {
        crate::profiler::caches::hit(crate::profiler::caches::Cache::BundlePlans);
        return Ok(plan);
    }
    crate::profiler::caches::miss(crate::profiler::caches::Cache::BundlePlans);
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut dom = decode_model(&bytes)?;
    drop(bytes);
    // (the server loads a bundle and sends it: its rotations as a client receives them)
    super::received::as_received(&mut dom);
    let plan = ModelPlan::new(reflection, &dom, dom.root().children());
    drop(dom);
    if let Some(key) = &key {
        if let Some(encoded) = plan.encode(key) {
            write_atomic(&cache, &encoded);
            drop(encoded);
            // (read back in place: the plan's pages become the file's)
            if let Some(mapped) = Mapping::open(&cache).and_then(|file| ModelPlan::decode_mapped(file, key)) {
                return Ok(mapped);
            }
        }
    }
    Ok(plan)
}

/// The plan in converted file `cache` when it was written for `key`: mapped and read in place where
/// the system allows (its pages are the file's, shared by every run reading it), else read.
fn read_plan(cache: &Path, key: &[u8]) -> Option<ModelPlan> {
    match Mapping::open(cache) {
        Some(file) => ModelPlan::decode_mapped(file, key),
        None => ModelPlan::decode(&std::fs::read(cache).ok()?, key),
    }
}

/// Where bundle file `path`'s converted plan is kept.
fn converted_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".plan");
    PathBuf::from(name)
}

/// What a converted file must have been written for: the conversion (build.rs digests its code,
/// the reflection and the dump it resolves names and classes with, and the decoders' versions: a
/// rebuild that changed none of them keeps every plan) and the bundle file as it is.
fn converted_key(path: &Path) -> Option<Vec<u8>> {
    let stamp = file_stamp(path)?;
    Some(format!("conversion {}|{}|{}", env!("REE_CONVERSION_DIGEST"), stamp.len, stamp.modified).into_bytes())
}

/// Bundle files decoded and converted on other threads, taken by the imports that need them (the
/// decoded instances, several times the size of their plan, never wait for the importing thread).
pub struct Decoding {
    state: Mutex<DecodingState>,
    ready: Condvar,
}

#[derive(Default)]
struct DecodingState {
    queue: VecDeque<String>,
    slots: FxHashMap<String, Slot>,
}

enum Slot {
    Queued(PathBuf),
    Running,
    Ready(PathBuf, Result<ModelPlan, String>),
}

impl Decoding {
    /// Decodes and converts `files` (key, model file) in their order on up to 8 threads; None when
    /// there are none.
    fn start(files: Vec<(String, PathBuf)>, reflection: Arc<Reflection>) -> Option<Arc<Decoding>> {
        if files.is_empty() {
            return None;
        }
        let threads = std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(4).clamp(1, 8).min(files.len());
        let mut state = DecodingState::default();
        for (key, path) in files {
            state.queue.push_back(key.clone());
            state.slots.insert(key, Slot::Queued(path));
        }
        let decoding = Arc::new(Decoding { state: Mutex::new(state), ready: Condvar::new() });
        for _ in 0..threads {
            let shared = decoding.clone();
            let reflection = reflection.clone();
            let _ = std::thread::Builder::new().name("ree-bundles".into()).spawn(move || shared.work(&reflection));
        }
        Some(decoding)
    }

    fn work(&self, reflection: &Reflection) {
        loop {
            let (key, path) = {
                let Ok(mut state) = self.state.lock() else { return };
                let Some(key) = state.queue.pop_front() else { return };
                match state.slots.get_mut(&key) {
                    Some(slot @ Slot::Queued(_)) => match std::mem::replace(slot, Slot::Running) {
                        Slot::Queued(path) => (key, path),
                        _ => unreachable!(),
                    },
                    _ => continue,
                }
            };
            let result = convert_file(reflection, &path);
            let Ok(mut state) = self.state.lock() else { return };
            state.slots.insert(key, Slot::Ready(path, result));
            self.ready.notify_all();
        }
    }

    /// Bundle `key`'s file and its decoding: waits while it is being decoded; None when it was
    /// never queued or no thread took it yet (the caller decodes it then).
    fn take(&self, key: &str) -> Option<(PathBuf, Result<ModelPlan, String>)> {
        let mut state = self.state.lock().ok()?;
        loop {
            match state.slots.get(key)? {
                Slot::Queued(_) => {
                    state.slots.remove(key);
                    return None;
                }
                Slot::Running => state = self.ready.wait(state).ok()?,
                Slot::Ready(..) => match state.slots.remove(key) {
                    Some(Slot::Ready(path, result)) => return Some((path, result)),
                    _ => return None,
                },
            }
        }
    }
}

/// An asset worker: a child process that answers one JSON request a line on its stdin with one JSON
/// reply a line on its stdout (`{"command":"catalog", ...}`: the manifest's bundle keys as `bundles`;
/// `{"command":"asset","uri":"rbxassetid://<id>?version=<v>","out":<file>}`: the asset downloaded into
/// `out`, its path as `file`; a reply with `error` is a failure), and handles any authentication the
/// downloads need itself. ree passes it nothing but the requests and `--offline`.
/// Its executable is `REE_ASSET_WORKER`.
/// A bundle file is
/// the asset version the manifest pins, as uploaded (the worker's `asset` command:
/// [`BundleShape::Uploaded`]; its `bundle` command writes only the children of the bundle's
/// instance), at `<state dir>/model-bundles/asset-<AssetId>-v<Version>.rbxm`: named by what it holds,
/// so a file that exists is current and one file serves every project folder that pins the version.
/// The files the `bundle` command wrote here before (`<sha256 of [project folder, key]>-<manifest line
/// digest>.rbxm`, the name roblox-ui-renderer's asset helper gives its own, and their plans) are never
/// read; each goes when its bundle is next used. Started on first use: the catalog and the bundle
/// files are reused while the manifest pins the same versions.
pub struct AssetWorker {
    repo: PathBuf,
    offline: bool,
    worker: Option<Worker>,
    manifest: Option<Manifest>,
    directory: Option<PathBuf>,
}

struct Worker {
    child: Child,
    input: ChildStdin,
    replies: Receiver<String>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How long one request may take (a first bundle download included).
const WORKER_TIMEOUT: Duration = Duration::from_secs(90);

/// The worker's failures, as warnings and errors name them
const WORKER_TIMED_OUT: &str = "the asset worker's request timed out";
const WORKER_EXITED: &str = "the asset worker exited";
const WORKER_ERROR: &str = "asset worker";
const WORKER_NO_MODEL: &str = "the asset worker returned no model file";
const WORKER_NO_ASSET: &str = "the asset worker returned no asset file";
const NOT_IN_MANIFEST: &str = "the asset manifest has no line of its own with";

impl AssetWorker {
    pub fn new(repo: &Path, offline: bool) -> Self {
        Self { repo: repo.to_path_buf(), offline, worker: None, manifest: None, directory: None }
    }

    /// The executable this source runs.
    pub fn executable(_repo: &Path) -> Result<PathBuf, String> {
        std::env::var_os("REE_ASSET_WORKER")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "manifest bundles need an asset worker: set REE_ASSET_WORKER to its executable (docs/engine.md, \"Asset bundles\")".to_string())
    }

    fn start(&self) -> Result<Worker, String> {
        let mut command = Command::new(Self::executable(&self.repo)?);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if self.offline {
            command.arg("--offline");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().map_err(|error| format!("starting the asset worker: {error}"))?;
        let input = child.stdin.take().ok_or("the asset worker has no stdin")?;
        let output = child.stdout.take().ok_or("the asset worker has no stdout")?;
        let (send, replies) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Worker { child, input, replies })
    }

    fn request(&mut self, request: serde_json::Value) -> Result<serde_json::Value, String> {
        if self.worker.is_none() {
            self.worker = Some(self.start()?);
        }
        let worker = self.worker.as_mut().expect("started");
        let sent = writeln!(worker.input, "{request}").and_then(|()| worker.input.flush());
        let reply = match sent {
            Ok(()) => match worker.replies.recv_timeout(WORKER_TIMEOUT) {
                Ok(line) => serde_json::from_str::<serde_json::Value>(&line).map_err(|error| error.to_string()),
                Err(RecvTimeoutError::Timeout) => Err(WORKER_TIMED_OUT.into()),
                Err(RecvTimeoutError::Disconnected) => Err(WORKER_EXITED.into()),
            },
            Err(error) => Err(format!("{WORKER_ERROR}: {error}")),
        };
        match reply {
            Ok(reply) if reply["error"].is_string() => Err(reply["error"].as_str().unwrap_or_default().to_string()),
            Ok(reply) => Ok(reply),
            Err(error) => {
                self.worker = None;
                Err(error)
            }
        }
    }
}

impl BundleSource for AssetWorker {
    /// The catalog of the manifest as it is: kept beside the bundle files
    /// (`catalog-<digest of the project folder>.json`, with the manifest's digest), so the worker starts only when the
    /// manifest changed.
    fn catalog(&mut self, project: &Path) -> Result<Vec<String>, String> {
        let manifest = self.manifest(project)?.digest.clone();
        let file = self.directory()?.join(format!("catalog-{}.json", short_digest(&serde_json::to_string(&json!([project])).unwrap_or_default())));
        if let Some(cached) = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()) {
            if cached["manifest"].as_str() == Some(manifest.as_str()) {
                if let Some(bundles) = cached["bundles"].as_array() {
                    return Ok(bundles.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
                }
            }
        }
        let reply = self.request(json!({ "command": "catalog", "project": project, "manifest": project.join(MANIFEST) }))?;
        let bundles: Vec<String> = reply["bundles"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        write_atomic(&file, json!({ "manifest": manifest, "bundles": bundles }).to_string().as_bytes());
        Ok(bundles)
    }

    fn bundle(&mut self, project: &Path, key: &str) -> Result<PathBuf, String> {
        let out = self.bundle_file(project, key)?;
        self.forget_members_file(project, key);
        if crate::files::is_file(&out) {
            return Ok(out);
        }
        let uri = self.manifest(project)?.uri(key).ok_or_else(|| format!("{NOT_IN_MANIFEST} {key}'s AssetId"))?;
        let reply = self.request(json!({ "command": "asset", "uri": uri, "out": out }))?;
        reply["file"].as_str().map(PathBuf::from).ok_or_else(|| WORKER_NO_MODEL.to_string())
    }

    fn cached(&mut self, project: &Path, key: &str) -> Option<PathBuf> {
        self.bundle_file(project, key).ok().filter(|file| crate::files::is_file(file))
    }

    fn shape(&self) -> BundleShape {
        BundleShape::Uploaded
    }
}

impl AssetWorker {
    /// Downloads asset `uri` (`rbxassetid://N`) into file `out` through the worker (its `asset`
    /// command): the KeyframeSequences AnimationTracks play.
    pub fn fetch_asset(&mut self, uri: &str, out: &Path) -> Result<PathBuf, String> {
        let reply = self.request(json!({ "command": "asset", "uri": uri, "out": out }))?;
        reply["file"].as_str().map(PathBuf::from).ok_or_else(|| WORKER_NO_ASSET.to_string())
    }

    fn directory(&mut self) -> Result<PathBuf, String> {
        if let Some(directory) = &self.directory {
            return Ok(directory.clone());
        }
        let directory = crate::state_dir(&self.repo).join("model-bundles");
        std::fs::create_dir_all(&directory).map_err(|error| format!("{}: {error}", directory.display()))?;
        self.directory = Some(directory.clone());
        Ok(directory)
    }

    /// `<state dir>/model-bundles/asset-<AssetId>-v<Version>.rbxm`: the asset version bundle `key`'s
    /// manifest line pins, as uploaded (a line without a version: `asset-<AssetId>-<line digest>.rbxm`).
    fn bundle_file(&mut self, project: &Path, key: &str) -> Result<PathBuf, String> {
        let name = self.manifest(project)?.file_name(key).ok_or_else(|| format!("{NOT_IN_MANIFEST} {key}'s AssetId"))?;
        Ok(self.directory()?.join(name))
    }

    /// Removes the file the worker's `bundle` command wrote for `key` (`<sha256 of [project folder, key]>-<manifest
    /// line digest>.rbxm`: the children of the bundle's instance) and its plan, which nothing reads now.
    fn forget_members_file(&mut self, project: &Path, key: &str) {
        let Ok(entry) = self.manifest(project).map(|manifest| manifest.entry(key).to_string()) else { return };
        let Ok(directory) = self.directory() else { return };
        let file = directory.join(format!("{}-{entry}.rbxm", bundle_digest(project, key)));
        let _ = std::fs::remove_file(converted_path(&file));
        let _ = std::fs::remove_file(file);
    }

    fn manifest(&mut self, project: &Path) -> Result<&Manifest, String> {
        if self.manifest.as_ref().is_none_or(|manifest| manifest.project != project) {
            self.manifest = Some(Manifest::read(project)?);
        }
        Ok(self.manifest.as_ref().expect("read"))
    }
}

/// The asset manifest a source reads bundles by: its digest, and each entry's own (the digest of
/// its line) with the asset and version the line pins.
struct Manifest {
    project: PathBuf,
    digest: String,
    entries: FxHashMap<String, ManifestLine>,
}

struct ManifestLine {
    digest: String,
    /// AssetId, and Version when the line pins one
    asset: Option<(u64, Option<u64>)>,
}

impl Manifest {
    fn read(project: &Path) -> Result<Manifest, String> {
        let path = project.join(MANIFEST);
        crate::files::note(&path);
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        let mut entries = FxHashMap::default();
        // ["Visuals/Hazards"] = { AssetId = 1, Version = 2, AssetVersionId = 3, Hash = "...", ... },
        for line in text.lines() {
            let Some((key, table)) = line.trim().strip_prefix("[\"").and_then(|rest| rest.split_once("\"] = ")) else { continue };
            if !key.contains('\\') && table.contains("AssetId") && table.contains("Version") {
                let asset = number_field(table, "AssetId").map(|id| (id, number_field(table, "Version")));
                entries.insert(key.to_string(), ManifestLine { digest: short_digest(table), asset });
            }
        }
        Ok(Manifest { project: project.to_path_buf(), digest: short_digest(&text), entries })
    }

    /// An entry's digest; the whole manifest's for an entry not on a line of its own.
    fn entry(&self, key: &str) -> &str {
        self.entries.get(key).map_or(&self.digest, |line| &line.digest)
    }

    /// The asset version an entry on a line of its own pins, as the asset worker names it (what
    /// `InsertService:LoadAssetVersion` loads):
    /// `rbxassetid://<AssetId>?version=<Version>`, or the asset's current version for a line without one.
    fn uri(&self, key: &str) -> Option<String> {
        Some(match self.entries.get(key)?.asset? {
            (asset, Some(version)) => format!("rbxassetid://{asset}?version={version}"),
            (asset, None) => format!("rbxassetid://{asset}"),
        })
    }

    /// The name of the file holding that asset version.
    fn file_name(&self, key: &str) -> Option<String> {
        let line = self.entries.get(key)?;
        Some(match line.asset? {
            (asset, Some(version)) => format!("asset-{asset}-v{version}.rbxm"),
            (asset, None) => format!("asset-{asset}-{}.rbxm", line.digest),
        })
    }
}

/// The number in field `name = <digits>` of a table's text (not a field whose name ends with it).
fn number_field(table: &str, name: &str) -> Option<u64> {
    let mut from = 0;
    while let Some(found) = table[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        if table[..at].chars().next_back().is_some_and(|before| before.is_ascii_alphanumeric() || before == '_') {
            continue;
        }
        let Some(value) = table[from..].trim_start().strip_prefix('=') else { continue };
        let digits: String = value.trim_start().chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
    }
    None
}

/// The string in field `name = "<text>"` of a table's text (not a field whose name ends with it).
fn string_field(table: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = table[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        if table[..at].chars().next_back().is_some_and(|before| before.is_ascii_alphanumeric() || before == '_') {
            continue;
        }
        let Some(value) = table[from..].trim_start().strip_prefix('=') else { continue };
        let Some(text) = value.trim_start().strip_prefix('"') else { continue };
        let text = &text[..text.find('"')?];
        return (!text.contains('\\')).then(|| text.to_string());
    }
    None
}

/// The manifest's aliases, `["<key>"] = { AliasOf = "<path>", ... },`: the model at that path, copied to the key's path
/// as the game's installer copies it. Each key comes with the path its aliases end at: an alias of an alias is followed,
/// and a loop stops where it closes.
fn aliases_in(project: &Path) -> FxHashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(project.join(MANIFEST)) else { return FxHashMap::default() };
    let direct: FxHashMap<String, String> = text
        .lines()
        .filter_map(|line| {
            let (key, table) = line.trim().strip_prefix("[\"").and_then(|rest| rest.split_once("\"] = "))?;
            (!key.contains('\\')).then_some(())?;
            Some((key.to_string(), string_field(table, "AliasOf")?))
        })
        .collect();
    direct
        .keys()
        .map(|key| {
            let mut at = key.as_str();
            let mut seen: FxHashSet<&str> = FxHashSet::default();
            while let Some(next) = direct.get(at) {
                if !seen.insert(at) {
                    break;
                }
                at = next;
            }
            (key.clone(), at.to_string())
        })
        .collect()
}

fn short_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

/// sha256 of JSON [project folder, key]: the names roblox-ui-renderer's asset helper gives bundle files
fn bundle_digest(project: &Path, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let name = serde_json::to_string(&json!([project, key])).unwrap_or_default();
    Sha256::digest(name.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    let _ = crate::files::write_atomic(path, bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bundles from a source: a model file per key
    struct Files(PathBuf);

    impl BundleSource for Files {
        fn catalog(&mut self, _: &Path) -> Result<Vec<String>, String> {
            Ok(vec!["Models/Clone".into(), "Models/Descendants".into(), "Models/Query".into()])
        }

        fn bundle(&mut self, _: &Path, _: &str) -> Result<PathBuf, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn cloning_and_enumerating_materialize_real_bundle_children() {
        use crate::{Options, lua};
        use rbx_dom_weak::{InstanceBuilder, types::Vector3};
        let directory = std::env::temp_dir().join(format!("ree-model-mount-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&directory).unwrap();
        let model_file = directory.join("model.rbxm");
        let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_child(InstanceBuilder::new("Part").with_name("RealPart").with_property("Size", Vector3::new(2., 3., 4.))));
        let mut bytes = Vec::new();
        rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
        std::fs::write(&model_file, bytes).unwrap();
        let engine = Engine::new(Options::default()).unwrap();
        let vm = mlua::Lua::new();
        lua::start(&vm, &engine).unwrap();
        let root = folder(&engine, engine.world().game, "TestModels");
        let mut pending = FxHashMap::default();
        for name in ["Clone", "Descendants", "Query"] {
            pending.insert(folder(&engine, root, name), vec![format!("Models/{name}")]);
        }
        engine.state.borrow_mut().project.model_assets = ModelAssets { source: Some(Box::new(Files(model_file))), project: directory.clone(), pending, ..Default::default() };
        let module = {
            let mut state = engine.state.borrow_mut();
            core::create_named(&mut state, &engine.reflection, engine.reflection.class_id("ModuleScript").unwrap(), "Test")
        };
        let env = lua::modules::environment(&vm, &engine, module).unwrap();
        env.set("Models", lua::instance_value(&vm, &engine, root).unwrap()).unwrap();
        vm.load(r#"
            local Copy = Models:FindFirstChild("Clone"):Clone()
            assert(Copy.RealPart.Size == Vector3.new(2, 3, 4))
            local Descendants = Models.Descendants:GetDescendants()
            assert(#Descendants == 1 and Descendants[1].Name == "RealPart")
            local Results = Models.Query:QueryDescendants("Part")
            assert(#Results == 1 and Results[1].Name == "RealPart")
        "#).set_environment(env).exec().unwrap();
        let mut state = engine.state.borrow_mut();
        assert!(state.project.model_assets.pending.is_empty());
        state.project.model_assets.source = None;
        drop(state);
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Bundles as uploaded (a model file per key), and the keys fetched in order
    struct Uploaded(FxHashMap<String, PathBuf>, Rc<RefCell<Vec<String>>>);

    impl BundleSource for Uploaded {
        fn catalog(&mut self, _: &Path) -> Result<Vec<String>, String> {
            Ok(self.0.keys().cloned().collect())
        }

        fn bundle(&mut self, _: &Path, key: &str) -> Result<PathBuf, String> {
            self.1.borrow_mut().push(key.to_string());
            self.0.get(key).cloned().ok_or_else(|| format!("no bundle {key}"))
        }

        fn shape(&self) -> BundleShape {
            BundleShape::Uploaded
        }
    }

    fn write_model(file: &Path, dom: &WeakDom) {
        let mut bytes = Vec::new();
        rbx_binary::to_writer(&mut bytes, dom, dom.root().children()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }

    /// A machine as uploaded: a Model with a PrimaryPart, attributes and tags.
    fn machine(name: &str) -> WeakDom {
        use rbx_dom_weak::{
            InstanceBuilder,
            types::{Attributes, Tags, Variant, Vector3},
        };
        let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
        let mut attributes = Attributes::new();
        attributes.insert("Reach".into(), Variant::Float64(5.0));
        let mut tags = Tags::new();
        tags.push("Machine");
        let model = dom.insert(dom.root_ref(), InstanceBuilder::new("Model").with_name(name).with_property("Attributes", attributes).with_property("Tags", tags));
        let controller = dom.insert(model, InstanceBuilder::new("AnimationController"));
        dom.insert(controller, InstanceBuilder::new("Animator"));
        let body = dom.insert(model, InstanceBuilder::new("MeshPart").with_name("Body").with_property("Size", Vector3::new(4., 5., 6.)));
        let bone = dom.insert(body, InstanceBuilder::new("Bone").with_name("Root"));
        dom.insert(bone, InstanceBuilder::new("Bone").with_name("Neck"));
        dom.get_by_ref_mut(model).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(body));
        dom
    }

    /// The instance at an uploaded bundle's path is the bundle's own, however a script reaches it:
    /// indexed, found, waited for, listed then touched, tested by class or listed among descendants,
    /// and it is the one instance a script holds; a folder bundle keeps its folder (and its values),
    /// a bundle at a path below it lands inside it, a loose bundle's members go into the folder above,
    /// one that is not one instance mounts in a folder as before, and a bundle nothing looked at is
    /// not fetched.
    #[test]
    fn an_uploaded_bundle_is_its_instance_at_its_path() {
        use crate::{Options, lua};
        use rbx_dom_weak::{
            InstanceBuilder,
            types::{Attributes, Variant},
        };
        let directory = scratch("uploaded");
        let machine_file = directory.join("machine.rbxm");
        write_model(&machine_file, &machine("Machine File"));
        let units_file = directory.join("units.rbxm");
        {
            let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
            let mut attributes = Attributes::new();
            attributes.insert("Version".into(), Variant::Float64(3.0));
            let units = dom.insert(dom.root_ref(), InstanceBuilder::new("Folder").with_name("Units").with_property("Attributes", attributes));
            let old = dom.insert(units, InstanceBuilder::new("Model").with_name("Old"));
            let torso = dom.insert(old, InstanceBuilder::new("Part").with_name("Torso"));
            dom.get_by_ref_mut(old).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(torso));
            write_model(&units_file, &dom);
        }
        let nested_file = directory.join("nested.rbxm");
        {
            let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
            let nested = dom.insert(dom.root_ref(), InstanceBuilder::new("Model").with_name("Nested"));
            dom.insert(nested, InstanceBuilder::new("Part").with_name("Head"));
            write_model(&nested_file, &dom);
        }
        let loose_file = directory.join("loose.rbxm");
        {
            let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
            let content = dom.insert(dom.root_ref(), InstanceBuilder::new("Folder").with_name("Content"));
            dom.insert(content, InstanceBuilder::new("Model").with_name("Sword"));
            dom.insert(content, InstanceBuilder::new("Part").with_name("Shield"));
            write_model(&loose_file, &dom);
        }
        let two_file = directory.join("two.rbxm");
        {
            let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
            dom.insert(dom.root_ref(), InstanceBuilder::new("Part").with_name("A"));
            dom.insert(dom.root_ref(), InstanceBuilder::new("Part").with_name("B"));
            write_model(&two_file, &dom);
        }
        let mut files: FxHashMap<String, PathBuf> = FxHashMap::default();
        for name in ["ByIndex", "ByFind", "ByWait", "ByList", "Untouched"] {
            files.insert(format!("Models/Machines/{name}"), machine_file.clone());
        }
        files.insert("Models/Classes/ByClass".into(), machine_file.clone());
        files.insert("Models/WhichIsA/ByWhichIsA".into(), machine_file.clone());
        files.insert("Models/Deep/Below/ByDescendants".into(), machine_file.clone());
        files.insert("Models/Units".into(), units_file);
        files.insert("Models/Units/Nested".into(), nested_file);
        files.insert("Models/Items/_Loose".into(), loose_file);
        files.insert("Models/Odd/Two".into(), two_file);
        let mut keys: Vec<String> = files.keys().cloned().collect();
        keys.sort();

        let engine = Engine::new(Options::default()).unwrap();
        let vm = mlua::Lua::new();
        lua::start(&vm, &engine).unwrap();
        let root = folder(&engine, engine.world().game, "Assets");
        let pending = place_all(&engine, root, keys.iter().map(String::as_str), BundleShape::Uploaded);
        let fetched = Rc::new(RefCell::new(Vec::new()));
        engine.state.borrow_mut().project.model_assets = ModelAssets { source: Some(Box::new(Uploaded(files, fetched.clone()))), shape: BundleShape::Uploaded, project: directory.clone(), pending, ..Default::default() };
        let untouched = {
            let state = engine.state.borrow();
            let machines = state.dom.find_child(state.dom.find_child(root, b"Models").unwrap(), b"Machines").unwrap();
            state.dom.find_child(machines, b"Untouched").unwrap()
        };
        let module = {
            let mut state = engine.state.borrow_mut();
            core::create_named(&mut state, &engine.reflection, engine.reflection.class_id("ModuleScript").unwrap(), "Test")
        };
        let env = lua::modules::environment(&vm, &engine, module).unwrap();
        env.set("Assets", lua::instance_value(&vm, &engine, root).unwrap()).unwrap();
        vm.load(r#"
            local Machines = Assets.Models.Machines
            local function Check(Machine, Name, Parent)
                assert(Machine.Name == Name, Name)
                assert(Machine.ClassName == "Model" and Machine:IsA("Model"), Name .. " is a " .. Machine.ClassName)
                assert(Machine.Parent == Parent, Name)
                assert(Machine.PrimaryPart == Machine.Body, Name)
                assert(Machine:GetAttribute("Reach") == 5 and Machine:HasTag("Machine"), Name)
                assert(#Machine:GetChildren() == 2 and Machine.Body.Root.Neck:IsA("Bone"), Name)
            end
            local ByIndex = Machines.ByIndex
            Check(ByIndex, "ByIndex", Machines)
            assert(Machines:FindFirstChild("ByIndex") == ByIndex and Machines:WaitForChild("ByIndex") == ByIndex)
            Check(Machines:FindFirstChild("ByFind"), "ByFind", Machines)
            Check(Machines:WaitForChild("ByWait"), "ByWait", Machines)
            local Listed
            for _, Child in Machines:GetChildren() do
                if Child.Name == "ByList" then
                    Listed = Child
                end
            end
            assert(Listed:IsA("Model") and Machines.ByList == Listed)
            Check(Listed, "ByList", Machines)
            Check(Assets.Models.Classes:FindFirstChildOfClass("Model"), "ByClass", Assets.Models.Classes)
            Check(Assets.Models.WhichIsA:FindFirstChildWhichIsA("Model"), "ByWhichIsA", Assets.Models.WhichIsA)
            local Found
            for _, Item in Assets.Models.Deep:GetDescendants() do
                if Item.Name == "ByDescendants" then
                    Found = Item
                end
            end
            Check(Found, "ByDescendants", Assets.Models.Deep.Below)
            local Copy = ByIndex:Clone()
            assert(Copy.ClassName == "Model" and Copy.PrimaryPart == Copy.Body and Copy:GetAttribute("Reach") == 5)
            assert(table.find(game:GetService("CollectionService"):GetTagged("Machine"), ByIndex))

            local Units = Assets.Models.Units
            assert(Units.ClassName == "Folder" and Units:GetAttribute("Version") == 3)
            assert(Units.Old.ClassName == "Model" and Units.Old.PrimaryPart == Units.Old.Torso)
            assert(Units.Nested.ClassName == "Model" and Units.Nested.Head:IsA("Part") and Units.Nested.Parent == Units)
            assert(#Units:GetChildren() == 2)

            local Items = Assets.Models.Items
            assert(Items.ClassName == "Folder" and Items.Sword.ClassName == "Model" and Items.Shield.ClassName == "Part")
            assert(Items:FindFirstChild("Content") == nil and #Items:GetChildren() == 2)

            local Two = Assets.Models.Odd.Two
            assert(Two.ClassName == "Folder" and Two.A:IsA("Part") and Two.B:IsA("Part"))
        "#).set_environment(env).exec().unwrap();

        let fetched = fetched.borrow();
        assert!(!fetched.iter().any(|key| key == "Models/Machines/Untouched"), "fetched only what was looked at: {fetched:?}");
        assert_eq!(fetched.len(), keys.len() - 1, "{fetched:?}");
        let mut state = engine.state.borrow_mut();
        let assets = &state.project.model_assets;
        assert_eq!(assets.pending.len(), 1, "the untouched bundle still waits");
        assert_eq!(assets.pending.get(&untouched).map(Vec::as_slice), Some(&["Models/Machines/Untouched".to_string()][..]));
        let placeholder = state.dom.at(untouched);
        assert!(placeholder.has(flags::BUNDLE_ROOT) && &*engine.reflection.class(placeholder.class).name == "Folder");
        state.project.model_assets.source = None;
        drop(state);
        let _ = std::fs::remove_dir_all(directory);
    }

    /// An alias line (`AliasOf`) is a copy of the model it names, made at the alias's path when a script asks for that path,
    /// as the game's installer makes it: found, waited for or indexed by name (its folders too), named as its path's last
    /// segment, its model's bundle imported for it and left in place. An alias of a model inside a folder bundle lands in
    /// that bundle's own folder, an alias of an alias is followed, an alias nothing asked for is not made (a listing does
    /// not make it), and an alias of a part, of a path no bundle holds or of itself is not made.
    #[test]
    fn an_alias_is_a_copy_of_the_model_it_names() {
        use crate::{Options, lua};
        use rbx_dom_weak::{InstanceBuilder, types::Variant};
        let directory = scratch("aliases");
        let file = directory.join(MANIFEST);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"return {
	["Models/Racing/Steed"] = { AliasOf = "Models/Stables/Horse", ModelHash = "1:2" },
	["Models/Racing/Pony"] = { AliasOf = "Models/Racing/Steed" },
	["Models/Racing/Shield"] = { AliasOf = "Models/Arena/Hall/Shield" },
	["Models/Racing/Missing"] = { AliasOf = "Models/Stables/Nothing" },
	["Models/Arena/Hall/Copy"] = { AliasOf = "Models/Arena/Hall/Giant" },
	["Models/Arena/Hall/Spare"] = { AliasOf = "Models/Arena/Hall/Giant" },
	["Models/Loop/A"] = { AliasOf = "Models/Loop/B" },
	["Models/Loop/B"] = { AliasOf = "Models/Loop/A" },
	["Models/Stables/Horse"] = { AssetId = 1, Version = 1 },
	["Models/Arena"] = { AssetId = 2, Version = 1 },
}
"#).unwrap();
        let aliases = aliases_in(&directory);
        assert_eq!(aliases.len(), 8);
        assert_eq!(aliases["Models/Racing/Pony"], "Models/Stables/Horse");
        assert_eq!(aliases["Models/Loop/A"], "Models/Loop/A");
        assert!(!aliases.contains_key("Models/Stables/Horse"));
        let horse_file = directory.join("horse.rbxm");
        write_model(&horse_file, &machine("Horse File"));
        let arena_file = directory.join("arena.rbxm");
        {
            let mut dom = WeakDom::new(InstanceBuilder::new("DataModel"));
            let arena = dom.insert(dom.root_ref(), InstanceBuilder::new("Folder").with_name("Arena"));
            let hall = dom.insert(arena, InstanceBuilder::new("Folder").with_name("Hall"));
            let giant = dom.insert(hall, InstanceBuilder::new("Model").with_name("Giant"));
            let torso = dom.insert(giant, InstanceBuilder::new("Part").with_name("Torso"));
            dom.get_by_ref_mut(giant).unwrap().properties.insert("PrimaryPart".into(), Variant::Ref(torso));
            dom.insert(hall, InstanceBuilder::new("Part").with_name("Shield"));
            write_model(&arena_file, &dom);
        }
        let mut files: FxHashMap<String, PathBuf> = FxHashMap::default();
        files.insert("Models/Stables/Horse".into(), horse_file);
        files.insert("Models/Arena".into(), arena_file);
        let mut keys: Vec<String> = files.keys().cloned().collect();
        keys.sort();

        let engine = Engine::new(Options::default()).unwrap();
        let vm = mlua::Lua::new();
        lua::start(&vm, &engine).unwrap();
        let root = folder(&engine, engine.world().game, "Assets");
        let pending = place_all(&engine, root, keys.iter().map(String::as_str), BundleShape::Uploaded);
        let fetched = Rc::new(RefCell::new(Vec::new()));
        engine.state.borrow_mut().project.model_assets = ModelAssets {
            source: Some(Box::new(Uploaded(files, fetched.clone()))),
            shape: BundleShape::Uploaded,
            project: directory.clone(),
            pending,
            root: Some(root),
            aliases,
            ..Default::default()
        };
        let module = {
            let mut state = engine.state.borrow_mut();
            core::create_named(&mut state, &engine.reflection, engine.reflection.class_id("ModuleScript").unwrap(), "Test")
        };
        let env = lua::modules::environment(&vm, &engine, module).unwrap();
        env.set("Assets", lua::instance_value(&vm, &engine, root).unwrap()).unwrap();
        vm.load(r#"
            assert(#Assets.Models:GetChildren() == 2, "a listing makes no alias or alias folder")
            local Racing = Assets.Models:FindFirstChild("Racing")
            assert(Racing and Racing.ClassName == "Folder", "the aliases' folder, asked for")
            local Steed = Racing:FindFirstChild("Steed")
            assert(Steed and Steed.ClassName == "Model" and Steed.Name == "Steed" and Steed.Parent == Racing, "the alias")
            assert(Steed.PrimaryPart == Steed.Body and Steed:GetAttribute("Reach") == 5 and Steed:HasTag("Machine"), "a copy of the horse")
            assert(Racing:FindFirstChild("Steed") == Steed, "made once")
            local Horse = Assets.Models.Stables.Horse
            assert(Horse ~= Steed and Horse.Name == "Horse" and Horse.PrimaryPart == Horse.Body, "the horse stays where it is")
            assert(Racing:WaitForChild("Pony").PrimaryPart.Name == "Body", "an alias of an alias")
            assert(Racing:FindFirstChild("Shield") == nil and Racing:FindFirstChild("Missing") == nil, "an alias of a part or of nothing")
            assert(#Racing:GetChildren() == 2, "the aliases asked for")
            local Arena = Assets.Models.Arena
            assert(Arena.ClassName == "Folder" and #Arena:GetChildren() == 1, "the bundle has its one hall")
            assert(#Arena.Hall:GetChildren() == 2, "the hall as the bundle holds it")
            local Copy = Arena.Hall.Copy
            assert(Copy.ClassName == "Model" and Copy.PrimaryPart == Copy.Torso and Copy ~= Arena.Hall.Giant, "an alias inside a folder bundle")
            assert(#Arena.Hall:GetChildren() == 3, "the hall holds the giant, the shield and the copy, not the spare")
            assert(Assets.Models.Loop:FindFirstChild("A") == nil and Assets.Models.Loop:FindFirstChild("B") == nil, "a loop")
        "#).set_environment(env.clone()).exec().unwrap();

        let mut fetched = fetched.borrow().clone();
        fetched.sort();
        assert_eq!(fetched, ["Models/Arena", "Models/Stables/Horse"], "each bundle fetched once");
        let mut state = engine.state.borrow_mut();
        assert!(state.project.model_assets.pending.is_empty());
        state.project.model_assets.source = None;
        drop(state);
        let _ = std::fs::remove_dir_all(directory);
    }

    /// A bundle's placeholder a listing handed out becomes the bundle's instance when its name and
    /// parent alone are not what a script reads: a write, a method call, any other property.
    #[test]
    fn a_listed_placeholder_becomes_the_instance_when_touched() {
        use crate::{Options, lua};
        let directory = scratch("touched");
        let machine_file = directory.join("machine.rbxm");
        write_model(&machine_file, &machine("Machine File"));
        let mut files: FxHashMap<String, PathBuf> = FxHashMap::default();
        for name in ["Named", "Written", "Called", "Read"] {
            files.insert(format!("Machines/{name}"), machine_file.clone());
        }
        let keys: Vec<String> = files.keys().cloned().collect();
        let engine = Engine::new(Options::default()).unwrap();
        let vm = mlua::Lua::new();
        lua::start(&vm, &engine).unwrap();
        let root = folder(&engine, engine.world().game, "Assets");
        let pending = place_all(&engine, root, keys.iter().map(String::as_str), BundleShape::Uploaded);
        let fetched = Rc::new(RefCell::new(Vec::new()));
        engine.state.borrow_mut().project.model_assets = ModelAssets { source: Some(Box::new(Uploaded(files, fetched.clone()))), shape: BundleShape::Uploaded, project: directory.clone(), pending, ..Default::default() };
        let module = {
            let mut state = engine.state.borrow_mut();
            core::create_named(&mut state, &engine.reflection, engine.reflection.class_id("ModuleScript").unwrap(), "Test")
        };
        let env = lua::modules::environment(&vm, &engine, module).unwrap();
        env.set("Assets", lua::instance_value(&vm, &engine, root).unwrap()).unwrap();
        vm.load(r#"
            local Listed = {}
            for _, Child in Assets.Machines:GetChildren() do
                Listed[Child.Name] = Child
                assert(Child.Parent == Assets.Machines)
            end
            Listed.Written.Name = "Renamed"
            assert(Listed.Written.ClassName == "Model" and Listed.Written.Name == "Renamed")
            assert(Listed.Called:GetAttribute("Reach") == 5)
            assert(Listed.Read.PrimaryPart.Name == "Body")
            assert(Assets.Machines.Renamed == Listed.Written)
        "#).set_environment(env).exec().unwrap();
        let mut fetched = fetched.borrow().clone();
        fetched.sort();
        assert_eq!(fetched, ["Machines/Called", "Machines/Read", "Machines/Written"], "a name read fetched nothing");
        engine.state.borrow_mut().project.model_assets.source = None;
        let _ = std::fs::remove_dir_all(directory);
    }

    /// The asset version a manifest line pins, as the asset worker names it: fields
    /// read whole (AssetVersionId is not Version) in any order, a missing version the current one.
    #[test]
    fn manifest_lines_name_the_asset_version_they_pin() {
        let project = scratch("manifest");
        let file = project.join(MANIFEST);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, r#"return {
	["Models/Props/Old Lamp"] = { AssetId = 120000000000001, Version = 1, AssetVersionId = 60000000000001, Hash = "78:sha256:1d9d", CloudEdited = true },
	["Models/Reordered"] = { AssetVersionId = 9, Version = 4, AssetId = 7 },
	["Models/Current"] = { AssetId = 8, AssetVersionId = 9 },
	["Models/Props/Lamp (Tall)"] = { AliasOf = "Models/Props/Lamp" },
}
"#).unwrap();
        let manifest = Manifest::read(&project).unwrap();
        assert_eq!(manifest.uri("Models/Props/Old Lamp").as_deref(), Some("rbxassetid://120000000000001?version=1"));
        assert_eq!(manifest.file_name("Models/Props/Old Lamp").as_deref(), Some("asset-120000000000001-v1.rbxm"));
        assert_eq!(manifest.uri("Models/Reordered").as_deref(), Some("rbxassetid://7?version=4"));
        assert_eq!(manifest.uri("Models/Current").as_deref(), Some("rbxassetid://8"));
        assert!(manifest.file_name("Models/Current").is_some_and(|name| name.starts_with("asset-8-") && name.ends_with(".rbxm") && !name.contains("-v")));
        assert_eq!(manifest.uri("Models/Props/Lamp (Tall)"), None);
        assert_eq!(manifest.file_name("Models/Missing"), None);
        let _ = std::fs::remove_dir_all(&project);
    }

    /// The bundle file names roblox-ui-renderer's asset helper (cli/viewports.mjs) gives the same
    /// bundle: sha256 of JSON.stringify([project, key])
    #[test]
    fn bundle_files_are_named_as_the_asset_helper_names_them() {
        use sha2::{Digest, Sha256};
        let name = serde_json::to_string(&json!([Path::new("C:\\work\\game"), "Visuals/Hazards"])).unwrap();
        assert_eq!(name, r#"["C:\\work\\game","Visuals/Hazards"]"#);
        let digest: String = Sha256::digest(name.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(digest, "ab9e9f3a7bb7da958fa8416ad3df4a07b625eb8f1f6c4ca0811c5fdc68a7760d");
        assert_eq!(bundle_digest(Path::new("C:\\work\\game"), "Visuals/Hazards"), digest);
    }

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("ree-{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn write_manifest(project: &Path, entries: &[(&str, u64, u64)]) {
        let file = project.join(MANIFEST);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let lines: Vec<String> = entries.iter().map(|(key, id, version)| format!("\t[\"{key}\"] = {{ AssetId = {id}, Version = {version}, Hash = \"x\" }},")).collect();
        std::fs::write(file, format!("return {{\n{}\n}}\n", lines.join("\n"))).unwrap();
    }

    /// A bundle file is named by the asset version its manifest line pins, as uploaded: a new version of
    /// one bundle is another file and the others keep theirs, every project folder pinning a version uses
    /// its one file, the catalog and files a worker wrote are used without it while the manifest holds,
    /// and a file of the children the worker's `bundle` command wrote is never read and goes once its
    /// bundle is used.
    #[test]
    fn bundle_files_follow_their_manifest_entries() {
        let (project, other_project, repo) = (scratch("project"), scratch("other-project"), scratch("repo"));
        write_manifest(&project, &[("Visuals/Hazards", 1, 3), ("Models/Units", 2, 7)]);
        write_manifest(&other_project, &[("Visuals/Hazards", 1, 3)]);
        let mut worker = AssetWorker::new(&repo, true);
        assert_eq!(worker.shape(), BundleShape::Uploaded);
        let hazards = worker.bundle_file(&project, "Visuals/Hazards").unwrap();
        let units = worker.bundle_file(&project, "Models/Units").unwrap();
        assert_eq!(hazards.file_name().unwrap(), "asset-1-v3.rbxm");
        assert_eq!(units.file_name().unwrap(), "asset-2-v7.rbxm");
        assert_eq!(worker.bundle_file(&other_project, "Visuals/Hazards").unwrap(), hazards, "one file for every checkout pinning the version");
        assert_eq!(Manifest::read(&project).unwrap().uri("Visuals/Hazards").as_deref(), Some("rbxassetid://1?version=3"));
        assert!(worker.cached(&project, "Visuals/Hazards").is_none(), "not written yet");
        // (the children of the bundle's instance, as the worker's `bundle` command wrote them)
        let entry = Manifest::read(&project).unwrap().entry("Visuals/Hazards").to_string();
        let members = worker.directory().unwrap().join(format!("{}-{entry}.rbxm", bundle_digest(&project, "Visuals/Hazards")));
        std::fs::write(&members, b"children").unwrap();
        std::fs::write(converted_path(&members), b"plan").unwrap();
        assert!(worker.cached(&project, "Visuals/Hazards").is_none(), "the children's file is not the bundle");
        std::fs::write(&hazards, b"bundle").unwrap();
        assert_eq!(worker.cached(&project, "Visuals/Hazards").as_deref(), Some(hazards.as_path()));
        assert_eq!(worker.bundle(&project, "Visuals/Hazards").unwrap(), hazards, "a current file is used without the worker");
        assert!(!members.exists() && !converted_path(&members).exists(), "the children's file and its plan went");

        let catalog = worker.directory().unwrap().join(format!("catalog-{}.json", short_digest(&serde_json::to_string(&json!([project])).unwrap())));
        std::fs::write(&catalog, json!({ "manifest": Manifest::read(&project).unwrap().digest, "bundles": ["Models/Units", "Visuals/Hazards"] }).to_string()).unwrap();
        assert_eq!(worker.catalog(&project).unwrap(), ["Models/Units", "Visuals/Hazards"]);

        write_manifest(&project, &[("Visuals/Hazards", 1, 4), ("Models/Units", 2, 7)]);
        let mut worker = AssetWorker::new(&repo, true);
        assert_ne!(worker.bundle_file(&project, "Visuals/Hazards").unwrap(), hazards, "a new version is another file");
        assert_eq!(worker.bundle_file(&project, "Models/Units").unwrap(), units, "the other bundles keep theirs");
        assert!(worker.cached(&project, "Visuals/Hazards").is_none());
        assert_eq!(worker.cached(&other_project, "Visuals/Hazards").as_deref(), Some(hazards.as_path()), "a checkout pinning the old version keeps it");
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&other_project);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A kept plan goes once its bundle file changes or is gone; one whose file holds stays.
    #[test]
    fn kept_plans_of_changed_files_are_dropped() {
        use rbx_dom_weak::InstanceBuilder;
        let directory = scratch("kept");
        let reflection = crate::Reflection::shared().unwrap();
        let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_child(InstanceBuilder::new("Part")));
        keep_bundles();
        let (kept, changed) = (directory.join("kept.rbxm"), directory.join("changed.rbxm"));
        for file in [&kept, &changed] {
            std::fs::write(file, b"bundle").unwrap();
            keep(file, Arc::new(ModelPlan::new(&reflection, &dom, dom.root().children())));
        }
        std::fs::write(&changed, b"a newer bundle").unwrap();
        forget_stale_bundles();
        assert!(kept_plan(&kept).is_some(), "unchanged");
        assert!(kept_plan(&changed).is_none(), "changed");
        std::fs::remove_file(&kept).unwrap();
        forget_stale_bundles();
        assert!(kept_plan(&kept).is_none(), "gone");
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Each scene decodes ahead what its own last run imported: one that imported nothing decodes nothing
    /// (its empty list is kept), and a scene's first run guesses what the mount's last run imported.
    #[test]
    fn a_scene_decodes_ahead_what_its_last_run_imported() {
        use crate::Options;
        let repo = scratch("scenes");
        let project = repo.join("project");
        let prefixes = vec!["Models".to_string()];
        let engine = Engine::new(Options::default()).unwrap();
        let run = |scene: &str, imported: &[&str]| -> Vec<String> {
            let (recent, lists) = recent_lists(&repo, &project, "ReplicatedStorage.Bundles", &prefixes, Some(scene));
            let decoded = ahead(&recent, &lists).cloned().unwrap_or_default();
            engine.state.borrow_mut().project.model_assets = ModelAssets { recent, scene: lists, imported: imported.iter().map(|key| key.to_string()).collect(), ..Default::default() };
            remember(&engine);
            decoded
        };
        assert!(run("hud", &["Models/A", "Models/B"]).is_empty(), "nothing ran before");
        assert_eq!(run("rigs", &[]), ["Models/A", "Models/B"], "a first run guesses the mount's last run");
        assert!(run("rigs", &[]).is_empty(), "the scene imported nothing last time");
        assert_eq!(run("hud", &["Models/B"]), ["Models/A", "Models/B"]);
        assert_eq!(run("hud", &["Models/B"]), ["Models/B"]);
        assert_eq!(run("lobby", &[]), ["Models/B"], "the mount's last run imported B");
        let (one, other) = (recent_file(&repo, &project, "M", &prefixes, Some("a")), recent_file(&repo, &project, "M", &prefixes, Some("b")));
        assert!(one.is_some() && one != other && one != recent_file(&repo, &project, "M", &prefixes, None));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Bundle files decoded and converted ahead are taken by the imports that need them; a bundle
    /// nobody queued is the caller's to decode.
    #[test]
    fn bundles_decode_ahead() {
        use rbx_dom_weak::InstanceBuilder;
        let directory = scratch("decoding");
        let mut files = Vec::new();
        for index in 0..6 {
            let dom = WeakDom::new(InstanceBuilder::new("DataModel").with_child(InstanceBuilder::new("Part").with_name(format!("Part{index}"))));
            let mut bytes = Vec::new();
            rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
            let file = directory.join(format!("{index}.rbxm"));
            std::fs::write(&file, bytes).unwrap();
            files.push((format!("Models/{index}"), file));
        }
        let reflection = crate::Reflection::shared().unwrap();
        let decoding = Decoding::start(files.clone(), reflection.clone()).expect("decoding");
        for (index, (key, file)) in files.iter().enumerate().rev() {
            match decoding.take(key) {
                Some((path, Ok(plan))) => {
                    assert_eq!(&path, file);
                    let names: Vec<&[u8]> = plan.root_names().collect();
                    assert_eq!(names, [format!("Part{index}").as_bytes()]);
                }
                Some((_, Err(error))) => panic!("{error}"),
                // not started yet: the import decodes it
                None => {}
            }
            assert!(decoding.take(key).is_none(), "taken once");
        }
        assert!(decoding.take("Models/Other").is_none());
        assert!(Decoding::start(Vec::new(), reflection).is_none());
        let _ = std::fs::remove_dir_all(&directory);
    }
}

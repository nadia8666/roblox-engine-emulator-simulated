//! Rojo project files (`*.project.json`): the tree a project describes, as the mounts it comes to
//! ([`crate::project::Mount`]: `--mount Service/Name=<path>` and `--instance Service/Name=<ClassName>`). `ree run` and
//! `ree record` mount one with `--rojo <file>` ([`Tree`], [`crate::launch`]). The module is shared with
//! roblox-ui-renderer's native commands, and its rules are those of that tool's Node CLI (cli/rojo.mjs), with Rojo's own
//! for a `$path` to a project file:
//!
//! - A DataModel tree (`"$className": "DataModel"`): each node with a `$path` mounts that directory or file at its
//!   instance path (ReplicatedStorage/Packages, StarterPlayer/StarterPlayerScripts/...); `$className` nodes without
//!   a `$path` below a service become instances of that class.
//! - Any other tree (a plugin or model project) is mounted at `ReplicatedStorage/<project name>`, or where
//!   `--rojo-at Service/Name` says.
//! - A node whose `$path` names another `*.project.json` is that project's tree, as Rojo builds it: the tree's class,
//!   `$path` and children (paths relative to that file), then the node's own children; the node's `$className` wins
//!   only over a Folder. The tree's root too, so a root that is a `$path` to a DataModel project, with more services
//!   beside it, is a DataModel tree. Project files whose `$path`s form a cycle are refused.
//! - `{ "optional": path }` is skipped when missing; `$properties`, `$attributes` and `$ignoreUnknownInstances` are
//!   ignored.
//!
//! What a mounted file or folder becomes (scripts by suffix, `init` files, `.model.json`, `.rbxm` / `.rbxmx`, JSON
//! modules, a folder's own `default.project.json`) is the mount's business ([`crate::project::mount_at`]), the same for a
//! file a project maps as for one below a `--roots` folder.
use std::path::{Component, Path, PathBuf};

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// The file name a folder's Rojo project has when it has several
pub const DEFAULT_PROJECT_FILE: &str = "default.project.json";

/// The folders Wally installs packages into (realms shared, server and dev)
const WALLY_FOLDERS: [&str; 3] = ["Packages", "ServerPackages", "DevPackages"];

/// The *.project.json files in `dir`, by name
pub fn project_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| is_project_file(&entry.file_name().to_string_lossy()) && entry.path().is_file())
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// The Rojo project a folder names by itself: its default.project.json, else its only *.project.json. `Err` lists
/// the folder's project files when there are several and none is default.project.json.
pub fn project_file_in(dir: &Path) -> Result<Option<PathBuf>, Vec<PathBuf>> {
    let default = dir.join(DEFAULT_PROJECT_FILE);
    if default.is_file() {
        return Ok(Some(default));
    }
    let mut files = project_files_in(dir);
    match files.len() {
        0 => Ok(None),
        1 => Ok(files.pop()),
        _ => Err(files),
    }
}

/// What to say about a folder with several project files and no default.project.json
pub fn several_projects(files: &[PathBuf]) -> String {
    let names: Vec<String> = files.iter().filter_map(|file| file.file_name()).map(|name| name.to_string_lossy().into_owned()).collect();
    let dir = files.first().and_then(|file| file.parent()).map(Path::display);
    format!("{} has several Rojo projects ({}), so none was mounted: pass --rojo <file> to mount one", dir.map(|dir| dir.to_string()).unwrap_or_default(), names.join(", "))
}

/// `wally install` when `source` is a Wally package folder of a project beside a wally.toml
pub fn wally_advice(project_dir: &Path, source: &Path) -> Option<String> {
    let name = source.file_name()?.to_string_lossy();
    (WALLY_FOLDERS.contains(&name.as_ref()) && project_dir.join("wally.toml").is_file()).then(|| format!("run `wally install` in {}", project_dir.display()))
}

/// `a/b` as an instance path (`a.b`)
pub fn dotted(path: &str) -> String {
    path.replace('/', ".")
}

/// `path` relative to `dir` when it is inside it
fn shown(path: &Path, dir: &Path) -> String {
    path.strip_prefix(dir).ok().filter(|rest| !rest.as_os_str().is_empty()).unwrap_or(path).display().to_string()
}

/// What to say about a `$path` that does not exist (and, for a Wally folder, how to make it)
pub fn missing_path(file: &Path, at: &str, source: &Path) -> String {
    let dir = file.parent().unwrap_or(Path::new(""));
    let name = file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let advice = wally_advice(dir, source).map(|advice| format!(": {advice}")).unwrap_or_default();
    format!("{name} maps {} to {}, which doesn't exist{advice}", dotted(at), shown(source, dir))
}

/// A JSON value with its objects' keys in file order (serde_json's own map sorts them, and the mount order is the
/// order instances are created in, which `GetChildren` shows)
enum Node {
    Object(Vec<(String, Node)>),
    String(String),
    Other,
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Node, D::Error> {
        struct NodeVisitor;
        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = Node;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("JSON")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
                let mut entries: Vec<(String, Node)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Node>()? {
                    // JSON.parse keeps a repeated key at its first place with the last value
                    match entries.iter_mut().find(|(name, _)| *name == key) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                Ok(Node::Object(entries))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
                while seq.next_element::<Node>()?.is_some() {}
                Ok(Node::Other)
            }
            fn visit_str<E>(self, value: &str) -> Result<Node, E> {
                Ok(Node::String(value.to_string()))
            }
            fn visit_string<E>(self, value: String) -> Result<Node, E> {
                Ok(Node::String(value))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Node, E> {
                Ok(Node::Other)
            }
            fn visit_unit<E>(self) -> Result<Node, E> {
                Ok(Node::Other)
            }
        }
        deserializer.deserialize_any(NodeVisitor)
    }
}

impl Node {
    fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Object(entries) => entries.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    fn string(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Node::String(value)) => Some(value),
            _ => None,
        }
    }

    /// The children a tree node names: its keys without `$`, with object values
    fn children(&self) -> impl Iterator<Item = (&String, &Node)> {
        let entries: &[(String, Node)] = match self {
            Node::Object(entries) => entries,
            _ => &[],
        };
        entries.iter().filter(|(name, value)| !name.starts_with('$') && matches!(value, Node::Object(_))).map(|(name, value)| (name, value))
    }
}

/// One mount of a project: `--mount path=value` (a directory or file) or `--instance path=value` (a class name)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub instance: bool,
    /// The instance path below the DataModel, `/`-separated, starting with a service
    pub path: String,
    pub value: String,
}

impl Mount {
    /// The engine arguments of this mount
    pub fn arguments(&self) -> [String; 2] {
        [if self.instance { "--instance" } else { "--mount" }.into(), format!("{}={}", self.path, self.value)]
    }

    /// The mount as the engine applies it ([`crate::project::mount_at`]), its instance names as the project spells them
    pub fn engine_mount(&self) -> crate::project::Mount {
        use crate::project::MountTarget;
        let target = if self.instance { MountTarget::Class(self.value.clone()) } else { MountTarget::Path(self.value.clone()) };
        crate::project::Mount { segments: self.path.split('/').map(str::to_string).collect(), target }
    }
}

/// Whether `file` names a Rojo project (`*.project.json`)
pub fn is_project_file(file: &str) -> bool {
    file.to_ascii_lowercase().ends_with(".project.json")
}

/// `path` made absolute against the current directory with `.` and `..` removed, as Node's `path.resolve` does
pub fn resolve(base: &Path, path: &str) -> PathBuf {
    let joined = base.join(path);
    let joined = std::path::absolute(&joined).unwrap_or(joined);
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // (never above the root, as path.resolve)
                if !matches!(out.components().next_back(), Some(Component::RootDir | Component::Prefix(_)) | None) {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn read_project(file: &Path) -> Result<Node, String> {
    // (a run prepared ahead checks the project files it read: record/prepare.rs)
    crate::files::note(file);
    let text = std::fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
    serde_json::from_str::<Node>(&text).map_err(|error| format!("{}: {error}", file.display()))
}

/// A top-level name of a DataModel tree that can be a service (the engine checks the rest)
fn is_service(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
}

/// The mounts of a project file, in the order cli/rojo.mjs gives them. `at` is `--rojo-at Service/Name` (for a tree
/// that is not a DataModel). A non-optional `$path` that is missing is skipped and reported in `warnings`
/// ([`missing_path`]; the caller adds its name in front).
pub fn mounts(project_file: &Path, at: Option<&str>, warnings: &mut Vec<String>) -> Result<Vec<Mount>, String> {
    mounts_with(project_file, at, warnings, &mut |path| Ok(path))
}

/// [`mounts`], each `$path` first handed to `source`, which can stand another file or folder in for it
/// (roblox-ui-renderer's `--git-ref`: the path as it is at a commit, extracted there)
pub fn mounts_with(
    project_file: &Path,
    at: Option<&str>,
    warnings: &mut Vec<String>,
    source: &mut dyn FnMut(PathBuf) -> Result<PathBuf, String>,
) -> Result<Vec<Mount>, String> {
    let walk = walk(project_file, at, source)?;
    warnings.extend(walk.missing.iter().map(|mount| missing_path(&walk.file, &mount.path, Path::new(&mount.value))));
    Ok(walk.mounts)
}

/// Every `$path` of a project file, also the ones that do not exist: (instance path `a/b`, file or folder, exists)
pub fn mappings(project_file: &Path) -> Result<Vec<(String, PathBuf, bool)>, String> {
    let mut unchanged = |path| Ok(path);
    let walk = walk(project_file, None, &mut unchanged)?;
    let present = walk.mounts.into_iter().filter(|mount| !mount.instance).map(|mount| (mount.path, PathBuf::from(mount.value), true));
    Ok(present.chain(walk.missing.into_iter().map(|mount| (mount.path, PathBuf::from(mount.value), false))).collect())
}

/// The instance paths a project's files and folders mount at, as a note lists them
pub fn mounted_list(mounts: &[Mount]) -> String {
    let places: Vec<String> = mounts.iter().filter(|mount| !mount.instance).map(|mount| dotted(&mount.path)).collect();
    if places.is_empty() { "nothing".to_string() } else { places.join(", ") }
}

fn walk<'a>(project_file: &Path, at: Option<&str>, source: &'a mut dyn FnMut(PathBuf) -> Result<PathBuf, String>) -> Result<Walk<'a>, String> {
    let file = resolve(Path::new(""), &project_file.to_string_lossy());
    let project = read_project(&file)?;
    let Some(tree @ Node::Object(_)) = project.get("tree") else {
        return Err(format!("{}: not a Rojo project (no \"tree\")", file.display()));
    };
    let mut walk = Walk { file: file.clone(), mounts: Vec::new(), missing: Vec::new(), source };
    let base = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut files = vec![(file.clone(), identity(&file))];
    // (the root's class is the one its project files give: a tree that is a `$path` to a DataModel project, with more
    // services beside it, is a DataModel tree)
    let tree = walk.build(tree, "tree", &base, &mut files)?;
    if tree.class.as_deref() == Some("DataModel") {
        for (name, child) in tree.children {
            if !is_service(&name) {
                return Err(format!("{}: {name} at the top of a DataModel tree is not a service", file.display()));
            }
            walk.place(child, &mut vec![name]);
        }
    } else {
        let mut place: Vec<String> = match at {
            Some(at) => at.split(['/', '\\']).filter(|part| !part.is_empty()).map(str::to_string).collect(),
            None => vec!["ReplicatedStorage".into(), project.string("name").filter(|name| !name.is_empty()).unwrap_or("Project").to_string()],
        };
        if place.len() < 2 {
            return Err(format!("--rojo-at {}: needs Service/Name", at.unwrap_or_default()));
        }
        walk.place(tree, &mut place);
    }
    Ok(walk)
}

/// How many project files deep a `$path` chain can go, as roblox-ui-renderer's cli/rojo.mjs stops
const MAX_PROJECT_NESTING: usize = 32;

/// `file` as the cycle check compares it: its real path, when it has one
fn identity(file: &Path) -> PathBuf {
    std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf())
}

/// A tree node as Rojo builds it, before it is placed: what its `$path` names, its class and its children
#[derive(Default)]
struct Built {
    class: Option<String>,
    source: Source,
    children: Vec<(String, Built)>,
}

/// What a node's `$path` (one that isn't a project file) names
#[derive(Default)]
enum Source {
    /// No `$path`: the node is an instance of its class
    #[default]
    None,
    /// A file or folder that exists, mounted at the node
    Found(PathBuf),
    /// A plain `$path` that doesn't exist, reported
    Missing(PathBuf),
    /// An optional `$path` that doesn't exist, left out quietly
    Skipped,
}

struct Walk<'a> {
    file: PathBuf,
    mounts: Vec<Mount>,
    /// The non-optional `$path`s that do not exist
    missing: Vec<Mount>,
    /// What each `$path` stands for ([`mounts_with`])
    source: &'a mut dyn FnMut(PathBuf) -> Result<PathBuf, String>,
}

impl Walk<'_> {
    /// Node `name` (its `$path`s relative to `base`) as Rojo builds it (snapshot_project_node in Rojo's
    /// src/snapshot_middleware/project.rs, 7.x): a `$path` to another project file makes the node that project's tree, its
    /// class, `$path` and children (paths relative to that file), and the node's own children follow those. `files` are the
    /// project files the node sits in, the outermost first.
    fn build(&mut self, node: &Node, name: &str, base: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<Built, String> {
        let mut built = Built { class: node.string("$className").map(str::to_string), ..Built::default() };
        let (path_value, optional) = match node.get("$path") {
            Some(Node::String(path)) => (Some(path.as_str()), false),
            Some(optional @ Node::Object(_)) => (optional.string("optional"), true),
            _ => (None, false),
        };
        if let Some(path_value) = path_value {
            let source = (self.source)(resolve(base, path_value))?;
            // (found or missing, as a run prepared ahead checks it again)
            crate::files::note(&source);
            if !source.exists() {
                built.source = if optional { Source::Skipped } else { Source::Missing(source) };
            } else if is_project_file(&source.to_string_lossy()) {
                let project = self.build_project(&source, files)?;
                built.class = self.combined_class(name, built.class, project.class, &source, files)?;
                built.source = project.source;
                built.children = project.children;
            } else {
                built.source = Source::Found(source);
            }
        }
        for (child_name, child) in node.children() {
            let child = self.build(child, child_name, base, files)?;
            built.children.push((child_name.clone(), child));
        }
        Ok(built)
    }

    /// The tree of project file `file`, which a `$path` names, as [`Walk::build`] builds it; nothing for a file without a
    /// tree. A file that is already one of `files` is refused: a project can't include itself.
    fn build_project(&mut self, file: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<Built, String> {
        let real = identity(file);
        if let Some(first) = files.iter().position(|(_, seen)| *seen == real) {
            let dir = self.file.parent().unwrap_or(Path::new(""));
            let cycle: Vec<String> = files[first..].iter().map(|(seen, _)| seen.as_path()).chain([file]).map(|seen| shown(seen, dir)).collect();
            return Err(format!("{}: the project files' $paths form a cycle ({}): a project can't include itself", self.file.display(), cycle.join(" -> ")));
        }
        if files.len() > MAX_PROJECT_NESTING {
            return Err(format!("{}: project nesting too deep", self.file.display()));
        }
        let project = read_project(file)?;
        let Some(tree @ Node::Object(_)) = project.get("tree") else {
            return Ok(Built::default());
        };
        let base = file.parent().map(Path::to_path_buf).unwrap_or_default();
        files.push((file.to_path_buf(), real));
        let built = self.build(tree, "tree", &base, files);
        files.pop();
        built
    }

    /// The class of node `name`, whose `$path` names project file `project`, by Rojo's rule (snapshot_project_node): the
    /// project's root class (`from_path`), or the node's own `$className` when the root's is a Folder or names none. Both
    /// with another root class is refused, as Rojo refuses it.
    fn combined_class(&self, name: &str, own: Option<String>, from_path: Option<String>, project: &Path, files: &[(PathBuf, PathBuf)]) -> Result<Option<String>, String> {
        match (own, from_path) {
            (Some(own), Some(from_path)) if from_path != "Folder" => {
                let file = files.last().map_or(self.file.as_path(), |(file, _)| file.as_path());
                let dir = file.parent().unwrap_or(Path::new(""));
                Err(format!("{}: {name} has \"$className\": \"{own}\" and a $path to {}, whose tree is a {from_path}: Rojo takes both only when the $path is a Folder", file.display(), shown(project, dir)))
            }
            (Some(own), _) => Ok(Some(own)),
            (None, from_path) => Ok(from_path),
        }
    }

    /// The mounts of built node `node` at instance path `segments`, then its children's below it
    fn place(&mut self, node: Built, segments: &mut Vec<String>) {
        match node.source {
            Source::Found(source) => self.mounts.push(Mount { instance: false, path: segments.join("/"), value: source.to_string_lossy().into_owned() }),
            Source::Missing(source) => self.missing.push(Mount { instance: false, path: segments.join("/"), value: source.to_string_lossy().into_owned() }),
            Source::Skipped => {}
            Source::None => {
                if let (Some(class), true) = (node.class, segments.len() > 1) {
                    self.mounts.push(Mount { instance: true, path: segments.join("/"), value: class });
                }
            }
        }
        for (name, child) in node.children {
            segments.push(name);
            self.place(child, segments);
            segments.pop();
        }
    }
}

/// A Rojo project file read for a run (`--rojo <file>`, [`crate::launch::prepare`]): the mounts its tree comes to, in
/// file order, as the engine applies them, and what to say about the `$path`s it maps that don't exist. The mount
/// ([`crate::mount::begin_project`]) applies them where `--roots` folders mount.
#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    /// The project file, absolute
    pub file: PathBuf,
    pub mounts: Vec<crate::project::Mount>,
    /// One line per non-optional `$path` that doesn't exist ([`missing_path`]); an optional one is left out quietly
    pub missing: Vec<String>,
}

impl Tree {
    /// The tree of project file `file` (relative to the current directory); `at` places a tree that is not a DataModel
    /// (`--rojo-at Service/Name`; else ReplicatedStorage/<project name>)
    pub fn read(file: &Path, at: Option<&str>) -> Result<Tree, String> {
        let file = resolve(Path::new(""), &file.to_string_lossy());
        if !file.is_file() {
            crate::files::note(&file);
            return Err(format!("Rojo project {}: no such file", file.display()));
        }
        let mut missing = Vec::new();
        let mounts = mounts(&file, at, &mut missing)?;
        Ok(Tree { mounts: mounts.iter().map(Mount::engine_mount).collect(), file, missing })
    }

    /// The project file's folder: the folder content paths are read from unless `--project` names one, and the place's
    /// root wherever ree looks a place up by its root folder ([`crate::mount::ProjectSpec::place_roots`])
    pub fn folder(&self) -> String {
        self.file.parent().unwrap_or(Path::new("")).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ree-rojo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A DataModel tree mounts its `$path`s at their instance paths in file order, `$className` nodes without a path
    /// as instances, nested projects in place; missing paths are skipped (reported unless optional)
    #[test]
    fn a_datamodel_tree_becomes_mounts_in_file_order() {
        let dir = temp_dir("datamodel");
        write(&dir.join("src/shared/init.luau"), "return {}");
        write(&dir.join("Packages/Fusion.luau"), "return {}");
        write(&dir.join("nested/inner.project.json"), r#"{"name":"inner","tree":{"$path":"lib"}}"#);
        write(&dir.join("nested/lib/a.luau"), "return 1");
        write(
            &dir.join("default.project.json"),
            r#"{"name":"game","tree":{"$className":"DataModel",
                "ReplicatedStorage":{"$className":"ReplicatedStorage","Shared":{"$path":"src/shared"},"Packages":{"$path":"Packages"},
                    "Inner":{"$path":"nested/inner.project.json"},"Remotes":{"$className":"Folder"}},
                "ServerStorage":{"Gone":{"$path":"missing"},"Maybe":{"$path":{"optional":"missing-too"}}}}}"#,
        );
        let mut warnings = Vec::new();
        let mounts = mounts(&dir.join("default.project.json"), None, &mut warnings).unwrap();
        let listed: Vec<(bool, &str, String)> = mounts
            .iter()
            .map(|mount| (mount.instance, mount.path.as_str(), if mount.instance { mount.value.clone() } else { Path::new(&mount.value).strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/") }))
            .collect();
        assert_eq!(
            listed,
            vec![
                (false, "ReplicatedStorage/Shared", "src/shared".into()),
                (false, "ReplicatedStorage/Packages", "Packages".into()),
                (false, "ReplicatedStorage/Inner", "nested/lib".into()),
                (true, "ReplicatedStorage/Remotes", "Folder".into()),
            ]
        );
        assert_eq!(warnings, ["default.project.json maps ServerStorage.Gone to missing, which doesn't exist"]);
        assert_eq!(mounted_list(&mounts), "ReplicatedStorage.Shared, ReplicatedStorage.Packages, ReplicatedStorage.Inner");
        assert_eq!(mounts[3].arguments(), [String::from("--instance"), "ReplicatedStorage/Remotes=Folder".into()]);
        // Every $path, also the missing ones (optional too), with whether it exists
        let mapped: Vec<(String, bool)> = mappings(&dir.join("default.project.json")).unwrap().into_iter().map(|(at, _, exists)| (at, exists)).collect();
        assert_eq!(mapped.iter().filter(|(_, exists)| !exists).map(|(at, _)| at.as_str()).collect::<Vec<_>>(), ["ServerStorage/Gone"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tree that is not a DataModel mounts at ReplicatedStorage/<name>, or at --rojo-at
    #[test]
    fn a_plugin_tree_mounts_under_its_name_or_rojo_at() {
        let dir = temp_dir("plugin");
        write(&dir.join("src/init.luau"), "return {}");
        write(&dir.join("plugin.project.json"), r#"{"name":"MyPlugin","tree":{"$path":"src"}}"#);
        let file = dir.join("plugin.project.json");
        let mut warnings = Vec::new();
        assert_eq!(mounts(&file, None, &mut warnings).unwrap()[0].path, "ReplicatedStorage/MyPlugin");
        assert_eq!(mounts(&file, Some("ServerStorage/Tools"), &mut warnings).unwrap()[0].path, "ServerStorage/Tools");
        assert!(mounts(&file, Some("ServerStorage"), &mut warnings).unwrap_err().contains("needs Service/Name"));
        write(&dir.join("bad.project.json"), r#"{"name":"x","tree":{"$className":"DataModel","lower":{}}}"#);
        assert!(mounts(&dir.join("bad.project.json"), None, &mut warnings).unwrap_err().contains("is not a service"));
        write(&dir.join("treeless.project.json"), r#"{"name":"x"}"#);
        assert!(mounts(&dir.join("treeless.project.json"), None, &mut warnings).unwrap_err().contains("not a Rojo project (no \"tree\")"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder's project file is its default.project.json, else its only *.project.json; several others are named
    #[test]
    fn a_folder_names_its_project_file_by_itself_unless_it_has_several() {
        let dir = temp_dir("project-file");
        assert_eq!(project_file_in(&dir), Ok(None));
        write(&dir.join("place.project.json"), "{}");
        assert_eq!(project_file_in(&dir), Ok(Some(dir.join("place.project.json"))));
        write(&dir.join("dev.project.json"), "{}");
        assert_eq!(project_file_in(&dir), Err(vec![dir.join("dev.project.json"), dir.join("place.project.json")]));
        assert_eq!(
            several_projects(&project_files_in(&dir)),
            format!("{} has several Rojo projects (dev.project.json, place.project.json), so none was mounted: pass --rojo <file> to mount one", dir.display())
        );
        write(&dir.join("default.project.json"), "{}");
        assert_eq!(project_file_in(&dir), Ok(Some(dir.join("default.project.json"))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Wally folder that doesn't exist yet says how to make it
    #[test]
    fn a_missing_wally_folder_says_to_run_wally_install() {
        let dir = temp_dir("wally");
        write(&dir.join("game.project.json"), r#"{"name":"g","tree":{"$className":"DataModel","ReplicatedStorage":{"Packages":{"$path":"Packages"}}}}"#);
        let mut warnings = Vec::new();
        mounts(&dir.join("game.project.json"), None, &mut warnings).unwrap();
        assert_eq!(warnings, ["game.project.json maps ReplicatedStorage.Packages to Packages, which doesn't exist"]);
        write(&dir.join("wally.toml"), "[package]");
        warnings.clear();
        mounts(&dir.join("game.project.json"), None, &mut warnings).unwrap();
        assert_eq!(warnings, [format!("game.project.json maps ReplicatedStorage.Packages to Packages, which doesn't exist: run `wally install` in {}", dir.display())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A run's tree: the engine's mounts (absolute paths, the project's instance names), the missing paths' lines, the
    /// project's folder; a file that isn't there is refused
    #[test]
    fn a_run_reads_a_project_file_into_engine_mounts() {
        let dir = temp_dir("tree");
        write(&dir.join("src/server/main.server.luau"), "print('server')");
        write(&dir.join("game.project.json"), r#"{"name":"g","tree":{"$className":"DataModel","ServerScriptService":{"Server":{"$path":"src/server"},"Gone":{"$path":"gone"}},"Workspace":{"Spawn":{"$className":"SpawnLocation"}}}}"#);
        let tree = Tree::read(&dir.join("game.project.json"), None).unwrap();
        use crate::project::{Mount as EngineMount, MountTarget};
        assert_eq!(
            tree.mounts,
            vec![
                EngineMount { segments: vec!["ServerScriptService".into(), "Server".into()], target: MountTarget::Path(dir.join("src").join("server").to_string_lossy().into_owned()) },
                EngineMount { segments: vec!["Workspace".into(), "Spawn".into()], target: MountTarget::Class("SpawnLocation".into()) },
            ]
        );
        assert_eq!(tree.missing, ["game.project.json maps ServerScriptService.Gone to gone, which doesn't exist"]);
        assert_eq!(Path::new(&tree.folder()), dir.as_path());
        assert!(Tree::read(&dir.join("none.project.json"), None).unwrap_err().contains("no such file"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The mounts of project file `file` as (instance, path, class or file relative to `dir`)
    fn listed(file: &Path, dir: &Path) -> Vec<(bool, String, String)> {
        let mut warnings = Vec::new();
        let mounts = mounts(file, None, &mut warnings).unwrap();
        assert_eq!(warnings, Vec::<String>::new());
        mounts
            .into_iter()
            .map(|mount| {
                let value = if mount.instance { mount.value } else { Path::new(&mount.value).strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/") };
                (mount.instance, mount.path, value)
            })
            .collect()
    }

    /// A tree whose root is a `$path` to a DataModel project, with a service beside it (a code project and a world synced
    /// on its own, combined by a root project), is that DataModel tree with the service added, as `rojo build` makes it:
    /// the referenced project's nodes first, their paths relative to its file, then the root's own, a nested project's
    /// tree mounting at its node
    #[test]
    fn a_root_path_to_a_datamodel_project_is_a_datamodel_tree() {
        let dir = temp_dir("root-path");
        write(&dir.join("src/shared/init.luau"), "return {}");
        write(&dir.join("src/server/Main.server.luau"), "print('server')");
        write(
            &dir.join("default.project.json"),
            r#"{"name":"code","tree":{"$className":"DataModel",
                "ReplicatedStorage":{"Shared":{"$path":"src/shared"},"Remotes":{"$className":"Folder"}},
                "ServerScriptService":{"Server":{"$path":"src/server"}}}}"#,
        );
        write(&dir.join("world/map/Spawn.model.json"), r#"{"className":"SpawnLocation"}"#);
        write(&dir.join("world/workspace.project.json"), r#"{"name":"world","tree":{"$className":"Workspace","Map":{"$path":"map"}}}"#);
        write(&dir.join("game.project.json"), r#"{"name":"game","tree":{"$path":"default.project.json","Workspace":{"$path":"world/workspace.project.json"}}}"#);
        assert_eq!(
            listed(&dir.join("game.project.json"), &dir),
            [
                (false, "ReplicatedStorage/Shared".into(), "src/shared".into()),
                (true, "ReplicatedStorage/Remotes".into(), "Folder".into()),
                (false, "ServerScriptService/Server".into(), "src/server".into()),
                (false, "Workspace/Map".into(), "world/map".into()),
            ]
        );
        // The same tree written out in full mounts the same
        write(
            &dir.join("full.project.json"),
            r#"{"name":"game","tree":{"$className":"DataModel",
                "ReplicatedStorage":{"Shared":{"$path":"src/shared"},"Remotes":{"$className":"Folder"}},
                "ServerScriptService":{"Server":{"$path":"src/server"}},
                "Workspace":{"$path":"world/workspace.project.json"}}}"#,
        );
        assert_eq!(listed(&dir.join("full.project.json"), &dir), listed(&dir.join("game.project.json"), &dir));
        // A child beside the root's $path must be a service, as in a DataModel tree written out
        write(&dir.join("bad.project.json"), r#"{"name":"bad","tree":{"$path":"default.project.json","lower":{}}}"#);
        assert!(mounts(&dir.join("bad.project.json"), None, &mut Vec::new()).unwrap_err().contains("lower at the top of a DataModel tree is not a service"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `$path` chain (a project file whose tree is a `$path` to another) is followed to its end, at the root and below
    /// it: each file's paths relative to that file, each file's own nodes after the ones it references. A tree that isn't
    /// a DataModel at the end of the chain stays a model project.
    #[test]
    fn a_path_chain_is_followed_to_its_end() {
        let dir = temp_dir("chain");
        write(&dir.join("code/src/init.luau"), "return {}");
        write(&dir.join("code/default.project.json"), r#"{"name":"code","tree":{"$className":"DataModel","ReplicatedStorage":{"Shared":{"$path":"src"}}}}"#);
        write(&dir.join("mid/tools/Hammer.luau"), "return {}");
        write(&dir.join("mid/mid.project.json"), r#"{"name":"mid","tree":{"$path":"../code/default.project.json","ServerStorage":{"Tools":{"$path":"tools"}}}}"#);
        write(&dir.join("lib/inner/a.luau"), "return 1");
        write(&dir.join("lib/outer.project.json"), r#"{"name":"outer","tree":{"$path":"inner.project.json"}}"#);
        write(&dir.join("lib/inner.project.json"), r#"{"name":"inner","tree":{"$path":"inner"}}"#);
        write(&dir.join("extra/b.luau"), "return 2");
        write(&dir.join("top.project.json"), r#"{"name":"top","tree":{"$path":"mid/mid.project.json","ReplicatedStorage":{"Extra":{"$path":"extra"},"Lib":{"$path":"lib/outer.project.json"}}}}"#);
        assert_eq!(
            listed(&dir.join("top.project.json"), &dir),
            [
                (false, "ReplicatedStorage/Shared".into(), "code/src".into()),
                (false, "ServerStorage/Tools".into(), "mid/tools".into()),
                (false, "ReplicatedStorage/Extra".into(), "extra".into()),
                (false, "ReplicatedStorage/Lib".into(), "lib/inner".into()),
            ]
        );
        write(&dir.join("plugin.project.json"), r#"{"name":"Plugin","tree":{"$path":"lib/outer.project.json"}}"#);
        assert_eq!(listed(&dir.join("plugin.project.json"), &dir), [(false, "ReplicatedStorage/Plugin".into(), "lib/inner".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A node's `$className` and the class of the project its `$path` names combine as Rojo combines them: the project
    /// root's class, the node's over a Folder, and both with another class refused
    #[test]
    fn a_path_to_a_project_takes_its_class_by_rojos_rule() {
        let dir = temp_dir("class");
        write(&dir.join("kit.project.json"), r#"{"name":"kit","tree":{"$className":"Folder","Handle":{"$className":"Part"}}}"#);
        write(&dir.join("tool.project.json"), r#"{"name":"tool","tree":{"$className":"Tool"}}"#);
        write(&dir.join("game.project.json"), r#"{"name":"g","tree":{"$className":"DataModel","ReplicatedStorage":{"Kit":{"$className":"Model","$path":"kit.project.json"},"Tool":{"$path":"tool.project.json"}}}}"#);
        assert_eq!(
            listed(&dir.join("game.project.json"), &dir),
            [(true, "ReplicatedStorage/Kit".into(), "Model".into()), (true, "ReplicatedStorage/Kit/Handle".into(), "Part".into()), (true, "ReplicatedStorage/Tool".into(), "Tool".into())]
        );
        write(&dir.join("bad.project.json"), r#"{"name":"g","tree":{"$className":"DataModel","ReplicatedStorage":{"Kit":{"$className":"Model","$path":"tool.project.json"}}}}"#);
        let error = mounts(&dir.join("bad.project.json"), None, &mut Vec::new()).unwrap_err();
        assert!(error.ends_with("bad.project.json: Kit has \"$className\": \"Model\" and a $path to tool.project.json, whose tree is a Tool: Rojo takes both only when the $path is a Folder"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Project files whose `$path`s come back to one of them are refused, naming the cycle, at the root and below it
    #[test]
    fn a_path_cycle_is_refused() {
        let dir = temp_dir("cycle");
        write(&dir.join("a.project.json"), r#"{"name":"a","tree":{"$className":"DataModel","ReplicatedStorage":{"B":{"$path":"b/b.project.json"}}}}"#);
        write(&dir.join("b/b.project.json"), r#"{"name":"b","tree":{"$path":"../a.project.json"}}"#);
        let error = mounts(&dir.join("a.project.json"), None, &mut Vec::new()).unwrap_err().replace('\\', "/");
        assert!(error.ends_with("a.project.json: the project files' $paths form a cycle (a.project.json -> b/b.project.json -> a.project.json): a project can't include itself"), "{error}");
        write(&dir.join("self.project.json"), r#"{"name":"self","tree":{"$path":"self.project.json","Workspace":{}}}"#);
        assert!(mounts(&dir.join("self.project.json"), None, &mut Vec::new()).unwrap_err().contains("form a cycle (self.project.json -> self.project.json)"));
        // A project referenced twice side by side is no cycle
        write(&dir.join("lib/x.luau"), "return 1");
        write(&dir.join("lib.project.json"), r#"{"name":"lib","tree":{"$path":"lib"}}"#);
        write(&dir.join("twice.project.json"), r#"{"name":"t","tree":{"$className":"DataModel","ReplicatedStorage":{"One":{"$path":"lib.project.json"},"Two":{"$path":"lib.project.json"}}}}"#);
        assert_eq!(listed(&dir.join("twice.project.json"), &dir), [(false, "ReplicatedStorage/One".into(), "lib".into()), (false, "ReplicatedStorage/Two".into(), "lib".into())]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

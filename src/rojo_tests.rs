//! `--rojo <file.project.json>` (src/rojo.rs, launch.rs, mount.rs): runs of a Rojo project file's tree through
//! `record::run`, as `ree run --rojo` makes them, read from their output.

use std::path::{Path, PathBuf};

use crate::{launch::Host, record::run};

/// A scratch folder of its own for a test
fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("ree-rojo-run-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
}

/// `ree run` of `words`: its summary and its output lines as `[side ]kind: message`
fn run_words(words: &[&str]) -> (serde_json::Value, Vec<String>) {
    let words: Vec<String> = words.iter().map(|word| word.to_string()).collect();
    let args = run::parse(&words, false).unwrap();
    let summary = run::run_only(&args, &Host::default()).unwrap();
    let lines = summary["output"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| {
            let side = line["side"].as_str().map(|side| format!("{side} ")).unwrap_or_default();
            format!("{side}{}: {}", line["kind"].as_str().unwrap_or("?"), line["message"].as_str().unwrap_or(""))
        })
        .collect();
    (summary, lines)
}

/// A game's place described by one project file whose tree is the DataModel (the shape roblox-ts and Rojo games use): its
/// `$path`s mount where Rojo puts them, as the files would below a `--roots` folder (a folder's init script makes it a
/// ModuleScript, `.server` / `.client` files make Scripts and LocalScripts, a `.model.json` one instance named by its node, a
/// folder under a node with no `$path` of its own, StarterPlayerScripts its class); `$className` nodes become instances, a
/// nested project file's tree mounts at its node; a require crosses the mapped tree; an optional `$path` that is missing is
/// left out quietly and a required one is reported; `$properties` change nothing
#[test]
fn a_rojo_project_mounts_its_tree_where_rojo_puts_it() {
    let root = scratch("tree");
    let game = root.join("game");
    write(
        &game,
        &[
            (
                "default.project.json",
                r#"{
	"name": "Sample",
	"tree": {
		"$className": "DataModel",
		"ReplicatedStorage": {
			"$className": "ReplicatedStorage",
			"Shared": { "$path": "src/shared" },
			"Lib": { "Deep": { "$path": "lib/deep" } },
			"Inner": { "$path": "inner/inner.project.json" },
			"Remotes": { "$className": "Folder", "Ping": { "$className": "RemoteEvent" } },
			"Maybe": { "$path": { "optional": "not-there" } },
			"Prop": { "$path": "models/Prop.model.json" }
		},
		"ServerScriptService": { "$className": "ServerScriptService", "Server": { "$path": "src/server" } },
		"ServerStorage": { "Gone": { "$path": "gone" } },
		"StarterPlayer": {
			"$className": "StarterPlayer",
			"StarterPlayerScripts": { "$className": "StarterPlayerScripts", "Client": { "$path": "src/client" } }
		},
		"Workspace": { "$className": "Workspace", "$properties": { "Gravity": 100 } }
	}
}"#,
            ),
            ("src/shared/init.luau", "return { Greeting = require(script.Parent.Lib.Deep.Words).Hello .. ' from shared' }"),
            ("lib/deep/Words.luau", "return { Hello = 'hello' }"),
            ("inner/inner.project.json", r#"{ "name": "Inner", "tree": { "$path": "src" } }"#),
            ("inner/src/Value.luau", "return 42"),
            ("models/Prop.model.json", r#"{ "className": "Part", "properties": { "Anchored": true }, "children": [{ "name": "Tag", "className": "StringValue" }] }"#),
            ("src/server/Main.server.luau", "print('server main')"),
            ("src/client/Main.client.luau", "print('client main')"),
        ],
    );
    write(
        &root,
        &[(
            "Harness.luau",
            "return function(Context)\n\
             local Storage = game:GetService('ReplicatedStorage')\n\
             print('shared', Storage.Shared.ClassName, require(Storage.Shared).Greeting)\n\
             print('inner', Storage.Inner.ClassName, require(Storage.Inner.Value))\n\
             print('remotes', Storage.Remotes.ClassName, Storage.Remotes.Ping.ClassName)\n\
             print('prop', Storage.Prop.ClassName, Storage.Prop.Anchored, Storage.Prop.Tag.ClassName)\n\
             print('maybe', Storage:FindFirstChild('Maybe'), game:GetService('ServerStorage'):FindFirstChild('Gone'))\n\
             local Server = game:GetService('ServerScriptService').Server\n\
             print('server', Server.ClassName, Server.Main.ClassName)\n\
             local Scripts = game:GetService('StarterPlayer').StarterPlayerScripts\n\
             print('client', Scripts.ClassName, Scripts.Client.ClassName, Scripts.Client.Main.ClassName)\n\
             print('gravity', workspace.Gravity)\n\
             local Names = {}\n\
             for _, Child in Storage:GetChildren() do table.insert(Names, Child.Name) end\n\
             print('order', table.concat(Names, ','))\n\
             end",
        )],
    );
    let harness = root.join("Harness.luau").to_string_lossy().into_owned();
    let project = game.join("default.project.json").to_string_lossy().into_owned();
    let (summary, lines) = run_words(&["--harness", &harness, "--rojo", &project, "--seconds", "0.1"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    for wanted in [
        "print: shared ModuleScript hello from shared",
        "print: inner Folder 42",
        "print: remotes Folder RemoteEvent",
        "print: prop Part true StringValue",
        "print: maybe nil nil",
        "print: server Folder Script",
        "print: client StarterPlayerScripts Folder LocalScript",
        // (in file order; the harness's own folder after them)
        "print: order Shared,Lib,Inner,Remotes,Prop,RURFixtures",
    ] {
        assert!(lines.iter().any(|line| line == wanted), "{wanted}: {lines:?}");
    }
    // the default gravity: $properties are not applied
    assert!(lines.iter().any(|line| line.starts_with("print: gravity ") && line != "print: gravity 100"), "{lines:?}");
    let warnings: Vec<&String> = lines.iter().filter(|line| line.starts_with("warn:")).collect();
    assert_eq!(warnings, ["warn: default.project.json maps ServerStorage.Gone to gone, which doesn't exist; not mounted"], "{lines:?}");
}

/// A root project whose tree is a `$path` to the code's DataModel project with the world's project beside it (a code project
/// and a world synced on its own) mounts both, as `rojo build` of it does: the code project's services from its own folder,
/// the world's Workspace from its folder
#[test]
fn a_root_project_combines_a_datamodel_project_and_a_workspace_project() {
    let root = scratch("root-path");
    write(
        &root,
        &[
            ("game.project.json", r#"{"name": "Game", "tree": {"$path": "default.project.json", "Workspace": {"$path": "world/workspace.project.json"}}}"#),
            (
                "default.project.json",
                r#"{"name": "Code", "tree": {"$className": "DataModel",
                    "ReplicatedStorage": {"Shared": {"$path": "src/shared"}},
                    "ServerScriptService": {"Server": {"$path": "src/server"}}}}"#,
            ),
            ("src/shared/Answer.luau", "return 7"),
            ("src/server/Main.server.luau", "print('server main')"),
            ("world/workspace.project.json", r#"{"name": "World", "tree": {"$className": "Workspace", "Map": {"$path": "map"}}}"#),
            ("world/map/Floor.model.json", r#"{"className": "Part", "properties": {"Anchored": true}}"#),
            (
                "Harness.luau",
                "return function(Context)\n\
                 print('shared', require(game:GetService('ReplicatedStorage').Shared.Answer))\n\
                 print('server', game:GetService('ServerScriptService').Server.Main.ClassName)\n\
                 print('map', workspace.Map.ClassName, workspace.Map.Floor.ClassName, workspace.Map.Floor.Anchored)\n\
                 end",
            ),
        ],
    );
    let harness = root.join("Harness.luau").to_string_lossy().into_owned();
    let project = root.join("game.project.json").to_string_lossy().into_owned();
    let (summary, lines) = run_words(&["--harness", &harness, "--rojo", &project, "--seconds", "0.05"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    for wanted in ["print: shared 7", "print: server Script", "print: map Folder Part true"] {
        assert!(lines.iter().any(|line| line == wanted), "{wanted}: {lines:?}");
    }
    assert!(!lines.iter().any(|line| line.starts_with("warn:") || line.starts_with("error:")), "{lines:?}");
}

/// A loopback run with no harness boots the place a Rojo project describes as a play session does: the server runs the
/// Script mapped into ServerScriptService, the client the LocalScript mapped into StarterPlayerScripts (copied into its
/// PlayerScripts), each requiring a module mapped into ReplicatedStorage
#[test]
fn a_rojo_place_boots_its_server_and_client_scripts() {
    let root = scratch("boot");
    write(
        &root,
        &[
            (
                "place.project.json",
                r#"{"name": "Boot", "tree": {"$className": "DataModel",
                    "ReplicatedStorage": {"Common": {"$path": "src/common"}},
                    "ServerScriptService": {"Server": {"$path": "src/server"}},
                    "StarterPlayer": {"StarterPlayerScripts": {"Client": {"$path": "src/client"}}}}}"#,
            ),
            ("src/common/Answer.luau", "return 7"),
            ("src/server/Main.server.luau", "print('server main', game:GetService('RunService'):IsServer(), require(game:GetService('ReplicatedStorage').Common.Answer))"),
            ("src/client/Main.client.luau", "print('client main', game:GetService('RunService'):IsClient(), require(game:GetService('ReplicatedStorage').Common.Answer), script.Parent.Parent.ClassName)"),
        ],
    );
    let project = root.join("place.project.json").to_string_lossy().into_owned();
    let (summary, lines) = run_words(&["--rojo", &project, "--seconds", "0.5", "--run-context", "loopback"]);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(summary["status"], "ok", "{lines:?}");
    for wanted in ["server print: server main true 7", "client print: client main true 7 PlayerScripts"] {
        assert_eq!(lines.iter().filter(|line| *line == wanted).count(), 1, "{wanted}: {lines:?}");
    }
}

/// A Rojo project file's folder stands for the place's root, as a `--roots` folder holding the place's project file
/// does: a server run gives the mounted scripts the Studio names of the project's name map
/// ([`crate::project::STUDIO_NAME_MAP`]), a client run keeps the file names
#[test]
fn a_rojo_project_folder_is_the_place_root_for_studio_names() {
    let root = scratch("names");
    write(
        &root,
        &[
            ("place/default.project.json", r#"{"name": "Place", "tree": {"$className": "DataModel", "ServerScriptService": {"$path": "ServerScriptService"}}}"#),
            ("place/ServerScriptService/Handlers/Gaze Stone.luau", "return 'gaze'"),
            (
                crate::project::STUDIO_NAME_MAP,
                r#"{ "version": 1, "files": { "place/ServerScriptService/Handlers/Gaze Stone.luau": { "studioPathParts": ["place", "ServerScriptService", "Handlers", "Gaze: Stone.luau"] } } }"#,
            ),
            (
                "Harness.luau",
                "return function(Context)\n\
                 local Names = {}\n\
                 for _, Child in game:GetService('ServerScriptService').Handlers:GetChildren() do table.insert(Names, Child.Name) end\n\
                 print('names', table.concat(Names, ','))\n\
                 end",
            ),
        ],
    );
    let harness = root.join("Harness.luau").to_string_lossy().into_owned();
    let project = root.join("place/default.project.json").to_string_lossy().into_owned();
    let mut printed = Vec::new();
    for run_context in ["server", "client"] {
        let (_, lines) = run_words(&["--harness", &harness, "--rojo", &project, "--seconds", "0.05", "--run-context", run_context]);
        printed.extend(lines.into_iter().map(|line| format!("{run_context} {line}")));
    }
    let _ = std::fs::remove_dir_all(&root);
    for wanted in ["server print: names Gaze: Stone", "client print: names Gaze Stone"] {
        assert!(printed.iter().any(|line| line == wanted), "{wanted}: {printed:?}");
    }
}

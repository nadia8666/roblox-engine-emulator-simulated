//! A library host keeps the paths and worker handoff it used before standalone binaries.
#![cfg(feature = "bundles")]

use std::{path::{Path, PathBuf}, process::Command};
use roblox_engine_emulator::{Engine, Options, animation::{self, Loading}};

// The CLI host links the same allocator collection.
extern crate mimalloc;

/// The test runs itself as a child with this variable set; the child is the library host.
const CHILD_MARKER: &str = "REE_TEST_HOST_CHILD";

/// The repo the child hands to `Options::repo`; without it the child uses `default_repo()`.
const CHILD_REPO: &str = "REE_TEST_HOST_REPO";

/// A node `--require` script that answers the worker protocol and logs each request to `requests.jsonl` in the worker's
/// working directory. It replies with the path relative to that directory when `out` is absolute, to show that a host's
/// worker is not moved to the state dir.
const MOCK_WORKER: &str = r#"
const fs = require("node:fs");
const path = require("node:path");
require("node:readline").createInterface({ input: process.stdin }).on("line", line => {
    const request = JSON.parse(line);
    fs.appendFileSync("requests.jsonl", JSON.stringify(request) + "\n");
    fs.mkdirSync(path.dirname(request.out), { recursive: true });
    fs.writeFileSync(request.out, '<roblox version="4"><Item class="KeyframeSequence" referent="RBX0"><Properties><string name="Name">Mock</string></Properties></Item></roblox>');
    console.log(JSON.stringify({ file: path.isAbsolute(request.out) ? path.relative(process.cwd(), request.out) : request.out }));
});
"#;

/// Whether the environment variable `name` (upper case) is one a service is keyed by.
fn keys_the_service(name: &str) -> bool {
    let prefixes = ["REE_", "RUR_", "ROR_"];
    prefixes.iter().any(|prefix| name.starts_with(prefix)) || name == "LOCALAPPDATA"
}

/// Whether the environment variable `name` (any case) steers ree, so the child starts without it.
fn steers_ree(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    let prefixes = ["REE_", "RUR_", "ROR_"];
    prefixes.iter().any(|prefix| name.starts_with(prefix))
}

/// The service key of a host: a hash of the environment values as they are, with no resolved paths. This is how
/// `env_digest` computed it before standalone binaries.
fn original_env_digest() -> String {
    use std::hash::{Hash, Hasher};
    let mut variables: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?.to_ascii_uppercase(), value.to_string_lossy().into_owned())))
        .filter(|(name, _)| keys_the_service(name))
        .collect();
    variables.sort();
    let mut hasher = rustc_hash::FxHasher::default();
    variables.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The child: a library host asks for one animation, which makes the engine start the worker.
fn run_as_library_host() {
    assert_eq!(roblox_engine_emulator::default_repo(), Path::new("."), "a library outside a checkout keeps the dot fallback");
    assert_eq!(roblox_engine_emulator::serve::env_digest(), original_env_digest(), "a host keys its service by the original environment values");
    let repo = std::env::var_os(CHILD_REPO).map(PathBuf::from).unwrap_or_else(roblox_engine_emulator::default_repo);
    let engine = Engine::new(Options { repo, ..Options::default() }).unwrap();
    animation::enable(&engine, Loading { download: true, ..Loading::default() });
    animation::source::sequence(&engine, "rbxassetid://6001").unwrap();
    animation::source::stop(&engine);
}

#[test]
fn a_library_host_keeps_relative_worker_paths() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        run_as_library_host();
        return;
    }

    // The child runs from a copy of this test binary in a temp folder, outside any checkout
    let root = std::env::temp_dir().join(format!("ree-host-worker-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let binary = root.join(if cfg!(windows) { "host-test.exe" } else { "host-test" });
    std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
    // The worker is node itself, turned into the mock by NODE_OPTIONS
    let node = Command::new("node").args(["-p", "process.execPath"]).output().unwrap();
    assert!(node.status.success());
    let worker = String::from_utf8(node.stdout).unwrap().trim().to_string();
    let mock = root.join("mock.cjs");
    std::fs::write(&mock, MOCK_WORKER).unwrap();

    // Each case: the repo the host names (none: the dot fallback) and the variable that makes the state dir relative
    let cases = [(None, None), (Some("repo"), None), (Some("repo"), Some("RUR_STATE_DIR")), (Some("repo"), Some("RUR_CACHE_DIR"))];
    for (index, (repo, state_variable)) in cases.into_iter().enumerate() {
        let directory = root.join(index.to_string());
        std::fs::create_dir_all(&directory).unwrap();
        let mut command = Command::new(&binary);
        for (name, _) in std::env::vars_os() {
            if steers_ree(&name.to_string_lossy()) {
                command.env_remove(name);
            }
        }
        command.current_dir(&directory).args(["--exact", "a_library_host_keeps_relative_worker_paths", "--nocapture"])
            .env(CHILD_MARKER, "1")
            .env("NODE_OPTIONS", format!("--require={}", serde_json::to_string(&mock).unwrap()));
        command.env("REE_ASSET_WORKER", &worker);
        if let Some(repo) = repo { command.env(CHILD_REPO, repo); }
        if let Some(variable) = state_variable { command.env(variable, "cache"); }
        let result = command.output().unwrap();
        assert!(result.status.success(), "case {index}: {}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));

        // The worker got the output path exactly as the library spells it, relative like the host's state dir
        let requests = std::fs::read_to_string(directory.join("requests.jsonl")).unwrap();
        let request: serde_json::Value = serde_json::from_str(requests.trim()).unwrap();
        let state = state_variable.map(|_| PathBuf::from("cache")).unwrap_or_else(|| Path::new(repo.unwrap_or(".")).join(".cache"));
        assert_eq!(Path::new(request["out"].as_str().unwrap()), state.join("animations/6001.rbxm"), "case {index}: the original relative output path");
    }
    std::fs::remove_dir_all(root).unwrap();
}

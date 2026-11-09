//! What a library host and ree's own binary each get as defaults.
//!
//! The defaults are once per process, so every case runs in a child process: `a_consumer_keeps_its_checkout_and_dot_fallback`
//! copies this test executable into a made-up host checkout and runs the other two tests there with an environment
//! variable that asks them to check. Run on their own (without the variable) they do nothing.

use std::{path::{Path, PathBuf}, process::{Command, Output}};

use roblox_engine_emulator::{Options, checkout_of, default_repo, is_standalone, launch::{self, Host}, serve::client::{absolute_paths, named_project}, set_default_repo, shared_state_path, state_dir};

// The engine's service collection uses mimalloc, as a CLI host does.
extern crate mimalloc;

/// The failure message of a child test: what it printed.
fn describe(output: &Output) -> String {
    format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// What a standalone client sends a service: every path the service reads or writes made absolute, as the service
/// runs in another folder.
fn assert_service_paths_are_absolute() {
    let args = |parts: &[&str]| parts.iter().map(|part| part.to_string()).collect::<Vec<String>>();

    assert_eq!(absolute_paths(&args(&["--frame-times", "frames.json"])), args(&["--frame-times", &launch::absolute("frames.json")]));
    // the other path flags share the conversion for runs, recordings and warm-up
    for flag in ["--harness", "--out", "--dir", "--dump-scene", "--project", "--snapshot", "--snapshot-dir", "--repo", "--context"] {
        assert_eq!(absolute_paths(&args(&[flag, "input"])), args(&[flag, &launch::absolute("input")]));
    }
    for flag in ["--roots", "--animations"] {
        let both = format!("{};{}", launch::absolute("first"), launch::absolute("second"));
        assert_eq!(absolute_paths(&args(&[flag, "first;second"])), args(&[flag, &both]));
    }
    let mount = format!("ReplicatedStorage/Lib={}", launch::absolute("input"));
    assert_eq!(absolute_paths(&args(&["--mount", "ReplicatedStorage/Lib=input"])), args(&["--mount", &mount]));
    // an inline context and an instance spec are values, not paths
    let inline = args(&["--context", "{\"value\":1}", "--instance", "ReplicatedStorage/Lib=Folder"]);
    assert_eq!(absolute_paths(&inline), inline);
}

/// Child process: ree's binary defaults, after `init_binary` finds no ree checkout above `REE_TEST_BINARY_SOURCE`.
#[test]
fn binary_defaults() {
    let Some(source) = std::env::var_os("REE_TEST_BINARY_SOURCE") else { return };
    let root = std::env::current_dir().unwrap();
    roblox_engine_emulator::init_binary(Path::new(&source), None).unwrap();

    // standalone: the empty repo, and the per-user cache of the platform (the parent redirected the folders into `root`)
    assert!(default_repo().as_os_str().is_empty());
    assert!(is_standalone(&Options::default().repo));
    let per_user_cache = match std::env::consts::OS {
        "windows" => root.join("local/ree"),
        "macos" => root.join("home/Library/Caches/ree"),
        _ => root.join("xdg/ree"),
    };
    assert_eq!(state_dir(&default_repo()), per_user_cache);

    // spooling script output writes a file too, even though it is deleted once closed, so it goes in state
    let mut output = roblox_engine_emulator::output::Output::default();
    assert!(output.spool());
    assert!(per_user_cache.join("output").is_dir(), "standalone output spools in state");

    assert_service_paths_are_absolute();

    // a launch has no implicit roots or snapshots, and its project is the current folder
    let prepared = launch::prepare(&Default::default(), &default_repo()).unwrap();
    assert_eq!(prepared.options.project, Some(root.clone()));
    assert!(prepared.roots.is_empty());
    assert!(prepared.snapshot_dir.is_none());

    // --repo only names input data: mode, state and the current-project default stay standalone
    let inputs = root.join("inputs");
    let repo_args: Vec<String> = vec!["--repo".into(), inputs.to_string_lossy().into_owned()];
    let mut args_with_project = repo_args.clone();
    args_with_project.extend(["--project".into(), root.to_string_lossy().into_owned()]);
    assert_eq!(named_project(repo_args), args_with_project);
    assert!(is_standalone(&inputs));
    assert_eq!(state_dir(&inputs), per_user_cache);
    let prepared = launch::prepare(&launch::parse(&args_with_project).unwrap(), &default_repo()).unwrap();
    assert_eq!(prepared.options.repo, inputs);
    assert_eq!(prepared.options.project, Some(root));
    assert!(prepared.roots.is_empty());
    assert!(prepared.snapshot_dir.is_none());
}

/// Child process: a host that depends on ree keeps the defaults it had, where `REE_TEST_HOST` is the repo they name.
#[test]
fn consumer_defaults() {
    let Some(expected) = std::env::var_os("REE_TEST_HOST") else { return };
    let expected = PathBuf::from(expected);
    assert_ne!(env!("CARGO_PKG_NAME"), "roblox-engine-emulator", "the host is not ree");

    // every default names the host's repo, and its cache is inside it
    assert_eq!(default_repo(), expected);
    assert_eq!(Options::default().repo, expected);
    assert_eq!(Host::default().repo, expected);
    assert_eq!(state_dir(&default_repo()), expected.join(".cache"));
    assert_eq!(shared_state_path(&default_repo(), "snapshots"), expected.join(".cache/snapshots"));
    assert_eq!(state_dir(Path::new("")), PathBuf::from(".cache"), "an empty host repo still means a relative cache");
    let frame_times = vec!["--frame-times".to_string(), "frames.json".to_string()];
    assert_eq!(absolute_paths(&frame_times), frame_times, "a host forwards --frame-times as it always did");

    // a host can replace the default repo
    let explicit = std::env::current_dir().unwrap();
    set_default_repo(explicit.clone());
    assert_eq!(default_repo(), explicit);
    assert_eq!(Options::default().repo, explicit);
    assert_eq!(Host::default().repo, explicit);
}

/// Child process: ree's binary with no cache folder anywhere. Making an engine directly gives the same plain error as a
/// launch does.
#[test]
fn binary_without_cache() {
    let Some(source) = std::env::var_os("REE_TEST_BINARY_WITHOUT_CACHE") else { return };
    roblox_engine_emulator::init_binary(Path::new(&source), None).unwrap();
    let expected = "cannot find a per-user cache folder; set RUR_STATE_DIR to a writable folder";
    assert_eq!(roblox_engine_emulator::Engine::new(Options::default()).err().unwrap(), expected);
    assert_eq!(launch::prepare(&Default::default(), &default_repo()).err().unwrap(), expected);
}

/// Copies this test executable into a made-up host checkout and runs three of its tests there: the host's defaults
/// with its `Cargo.lock`, and the same without it (the dot fallback), then the binary's defaults, with its per-user
/// cache folder and without one.
#[test]
fn a_consumer_keeps_its_checkout_and_dot_fallback() {
    // (kept when the test fails, so the files can be looked at)
    let root = std::env::temp_dir().join(format!("ree-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // the temporary folder may be reached through a link (macOS /var is /private/var), while the child reports real paths
    let root = std::fs::canonicalize(root).unwrap();
    // Windows canonical paths have a verbatim prefix (\\?\C:\...), while current_dir and child tools report ordinary paths
    #[cfg(windows)]
    let root = {
        let text = root.to_string_lossy();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
    };
    let host = root.join("host");
    let binary = host.join(format!("target/release/host{}", std::env::consts::EXE_SUFFIX));
    std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
    std::fs::write(host.join("Cargo.toml"), "[package]\nname = \"ree-test-host\"\n").unwrap();
    std::fs::write(host.join("Cargo.lock"), "").unwrap();
    assert_eq!(checkout_of(&binary), Some(host.clone()));

    // the child runs from `root`, with no cache folder chosen by the environment
    let child = |test: &str| {
        let mut command = Command::new(&binary);
        command.args(["--exact", test, "--nocapture"]).env_remove("RUR_STATE_DIR").env_remove("RUR_CACHE_DIR").current_dir(&root);
        command
    };
    let run_consumer_defaults = |expected_repo: &Path| {
        let output = child("consumer_defaults").env("REE_TEST_HOST", expected_repo).output().unwrap();
        assert!(output.status.success(), "{}", describe(&output));
    };

    // with a lockfile above the executable the repo is that checkout, without one it is `.`
    run_consumer_defaults(&host);
    std::fs::remove_file(host.join("Cargo.lock")).unwrap();
    assert_eq!(checkout_of(&binary), None);
    run_consumer_defaults(Path::new("."));

    // ree's own binary, with the per-user folders redirected into the made-up root
    let output = child("binary_defaults")
        .env("REE_TEST_BINARY_SOURCE", &binary)
        .env("LOCALAPPDATA", root.join("local"))
        .env("HOME", root.join("home"))
        .env("XDG_CACHE_HOME", root.join("xdg"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", describe(&output));

    // the same binary where the environment names no per-user folder at all
    let output = child("binary_without_cache")
        .env("REE_TEST_BINARY_WITHOUT_CACHE", &binary)
        .env_remove("LOCALAPPDATA")
        .env_remove("HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("USERPROFILE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    std::fs::remove_dir_all(root).unwrap();
}

/// The consumer checks must also work when the temporary folder is reached through a link (a symlink, or a junction on
/// Windows): the child test gets a link to a real folder as its temporary folder, and must leave nothing in the real one.
#[test]
fn consumer_checks_accept_a_linked_temporary_folder() {
    // node makes the link `argv[2]` to the folder `argv[1]`: a Windows junction needs no privilege, a symlink would
    const MAKE_LINK: &str = "require('node:fs').symlinkSync(process.argv[1], process.argv[2], process.platform === 'win32' ? 'junction' : 'dir')";
    let root = std::env::temp_dir().join(format!("ree-consumer-linked-{}", std::process::id()));
    let real = root.join("real");
    let linked = root.join("linked temp");
    std::fs::create_dir_all(&real).unwrap();
    let setup = Command::new("node").arg("-e").arg(MAKE_LINK).arg(&real).arg(&linked).output().unwrap();
    assert!(setup.status.success(), "{}", describe(&setup));

    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "a_consumer_keeps_its_checkout_and_dot_fallback", "--nocapture"])
        .env("TMPDIR", &linked)
        .env("TMP", &linked)
        .env("TEMP", &linked)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0, "passing consumer checks clean up through the link");
    // the link first, so that removing the root does not follow it: a Unix symlink is removed as a file, a Windows junction as a directory
    #[cfg(unix)]
    std::fs::remove_file(&linked).unwrap();
    #[cfg(windows)]
    std::fs::remove_dir(&linked).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

//! The same offline smoke as a release runner, with only ree copied outside its build checkout.

use std::path::Path;
use std::process::{Command, Output};

// The library's service collection links mimalloc, as the CLI host does.
extern crate mimalloc;

/// Runs `node <script> <args>` with the temporary folder moved to `temp`, so a test can see what the script leaves there.
/// The scripts that copy and run ree take the binary under test from `REE_TEST_BINARY`.
fn node(script: &str, args: &[&str], temp: &Path) -> Output {
    Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(script))
        .args(args)
        .env("TMPDIR", temp).env("TMP", temp).env("TEMP", temp)
        .env("REE_TEST_BINARY", env!("CARGO_BIN_EXE_ree"))
        .output()
        .expect("Node.js runs the script")
}

fn describe(output: &Output) -> String {
    format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// Prints the lines where a script gave up removing its temporary folder, and says whether there were any.
///
/// On Windows a process can hold a folder briefly after it exits. The script retries, then keeps the folder and says so.
/// That must not fail a test whose checks passed, but the test must then leave the folder alone and not count its files.
fn report_deferred_cleanup(output: &Output) -> bool {
    let diagnostic = describe(output);
    let mut deferred = false;
    for line in diagnostic.lines().filter(|line| line.contains("cleanup could not remove ")) {
        eprintln!("{line}");
        deferred = true;
    }
    deferred
}

// The only test here that reads or sets the default repo: both are once per process.
#[test]
fn a_host_can_replace_the_detected_checkout_with_its_own_repo() {
    use roblox_engine_emulator::{Options, default_repo, set_default_repo};
    let host = std::env::temp_dir().join("ree-host-checkout");
    set_default_repo(host.clone());
    assert_eq!(default_repo(), host);
    assert_eq!(Options::default().repo, host, "the options default to the host's repo");
    let other = std::env::temp_dir().join("ree-other-host-checkout");
    assert_eq!(Options { repo: other.clone(), ..Options::default() }.repo, other, "a host can still pass a repo per run");
}

#[test]
fn an_installed_binary_and_its_service_use_only_the_redirected_user_cache() {
    let temp = std::env::temp_dir().join(format!("ree-smoke-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    // the smoke compares the paths it made with the ones the binary reports, so the root must not be a link
    let temp = std::fs::canonicalize(temp).unwrap();
    let binary = env!("CARGO_BIN_EXE_ree");

    // a passing smoke removes its folder
    let passed = node("tools/release/smoke.mjs", &["--binary", binary, "--version", env!("CARGO_PKG_VERSION")], &temp);
    assert!(passed.status.success(), "{}", describe(&passed));
    let cleanup_deferred = report_deferred_cleanup(&passed);
    if !cleanup_deferred {
        assert_eq!(std::fs::read_dir(&temp).unwrap().count(), 0, "a passing smoke must clean {}", temp.display());
    }

    // a failing smoke (the wrong version) keeps its folder and says where it is
    let failed = node("tools/release/smoke.mjs", &["--binary", binary, "--version", "wrong-version"], &temp);
    assert!(!failed.status.success());
    // the folder of the passing run may still be there too (when its removal was deferred), so any entry may be the one named
    let diagnostic = String::from_utf8_lossy(&failed.stderr);
    let names_a_kept_folder = std::fs::read_dir(&temp).unwrap().any(|entry| {
        let kept = entry.unwrap().path();
        diagnostic.contains(&format!("files kept at {}", kept.display()))
    });
    assert!(names_a_kept_folder);
    if !cleanup_deferred {
        std::fs::remove_dir_all(&temp).unwrap();
    }
}

#[cfg(feature = "bundles")]
#[test]
fn asset_workers_keep_the_clients_paths_through_the_service() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/asset_worker.mjs");
    let result = Command::new("node").arg("--test").arg(script)
        .env("REE_TEST_BINARY", env!("CARGO_BIN_EXE_ree"))
        .output().expect("Node.js runs the asset worker tests");
    assert!(result.status.success(), "{}", describe(&result));
    report_deferred_cleanup(&result);
}

#[test]
fn installed_jobs_keep_standalone_state_with_an_input_repo() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/service_cache.mjs");
    let result = Command::new("node").arg("--test").arg(script)
        .env("REE_TEST_BINARY", env!("CARGO_BIN_EXE_ree"))
        .output().expect("Node.js runs the service cache tests");
    assert!(result.status.success(), "{}", describe(&result));
    report_deferred_cleanup(&result);
}

#[test]
fn standalone_checks_accept_a_linked_temporary_folder() {
    // Some systems reach their temporary folder through a link (macOS /var, a Windows junction). The checks build their
    // own paths in it and compare them with what the binary reports, so they must work when the folder is a link.
    let root = std::env::temp_dir().join(format!("ree-linked-temp-{}", std::process::id()));
    let real = root.join("real");
    let linked = root.join("linked temp");
    std::fs::create_dir_all(&real).unwrap();
    // A junction needs no elevated privileges on Windows; Unix uses a directory symlink.
    let link_script = "require('node:fs').symlinkSync(process.argv[1], process.argv[2], process.platform === 'win32' ? 'junction' : 'dir')";
    let setup = Command::new("node").arg("-e").arg(link_script).arg(&real).arg(&linked).output().unwrap();
    assert!(setup.status.success(), "{}", describe(&setup));

    let smoke_args = ["--binary", env!("CARGO_BIN_EXE_ree"), "--version", env!("CARGO_PKG_VERSION")];
    let mut checks: Vec<(&str, &[&str])> = vec![("tests/service_cache.mjs", &[]), ("tools/release/smoke.mjs", &smoke_args)];
    // the asset worker needs the bundles feature
    if cfg!(feature = "bundles") {
        checks.push(("tests/asset_worker.mjs", &[]));
    }
    let mut cleanup_deferred = false;
    for (script, args) in checks {
        let result = node(script, args, &linked);
        assert!(result.status.success(), "{script}: {}", describe(&result));
        cleanup_deferred |= report_deferred_cleanup(&result);
    }
    if !cleanup_deferred {
        assert_eq!(std::fs::read_dir(&real).unwrap().count(), 0, "passing checks clean up through the link");
    }
    // the link first, so that removing the root does not follow it: a Unix symlink is removed as a file, a Windows junction as a directory
    #[cfg(unix)]
    std::fs::remove_file(&linked).unwrap();
    #[cfg(windows)]
    std::fs::remove_dir(&linked).unwrap();
    if !cleanup_deferred {
        std::fs::remove_dir_all(&root).unwrap();
    }
}

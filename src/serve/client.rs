//! `ree record --service ...` / `ree run --service ...`: the command run by the service of this
//! build and environment ([`super`]) when one runs, or when jobs come in a burst ([`BURST`]): then it
//! is started (once, under a lock, detached from the caller) when missing. The client prints what the
//! command printed and exits with its code. It runs the command itself otherwise (a lone job), when
//! the service cannot be reached, and for a Luau profile (RUR_LUAU_PROFILE or
//! RUR_LUAU_ALLOC_PROFILE, sampled in the process that runs the VM).

use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use serde_json::json;

use super::{ServiceInfo, build_id, env_digest, http, record_path, services_dir};
use crate::{launch, record::run as command};

const START_TIMEOUT: Duration = Duration::from_secs(60);

/// `ree record|run --service <args>`: the exit code.
pub fn cli(argv: &[String], record: bool) -> i32 {
    let started = Instant::now();
    let args: Vec<String> = launch::split_equals(argv).into_iter().filter(|arg| arg != "--service").collect();
    let host = launch::Host::default();
    // the client waits for the service's reply no longer than a run of its own would run (watchdog.rs)
    if let Err(error) = command::guard_process(if record { "record" } else { "run" }, &args, started) {
        eprintln!("{} {}: {error}", host.name, if record { "record" } else { "run" });
        return 1;
    }
    // (a profile is of this process: the client runs the command itself)
    if ["RUR_LUAU_PROFILE", "RUR_LUAU_ALLOC_PROFILE"].iter().any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty())) || crate::profiler::requested(&args) {
        return command::cli(&args, &host, record);
    }
    let Some(state_dir) = super::default_state_dir("ree") else { return 2 };
    let name = if record { "record" } else { "run" };
    let body = json!({ "command": name, "args": absolute_paths(&named_project(args.clone())) }).to_string();
    let (build, env) = (build_id(), env_digest());
    let burst = burst(&state_dir, &build, &env);
    // the recorded service first, without a health check (a refused or foreign answer falls back
    // to finding or starting one, in a burst of jobs)
    let recorded = read_info(&record_path(&state_dir, &build, &env)).ok_or_else(String::new).and_then(|info| post(info.port, body.as_bytes()));
    let replied = match recorded {
        Ok(reply) => Ok(reply),
        Err(_) if burst => ensure(&state_dir).and_then(|port| post(port, body.as_bytes())),
        Err(_) => {
            super::size_pool();
            return command::cli(&args, &host, record);
        }
    };
    match replied {
        Ok(reply) => {
            if args.iter().any(|arg| arg == "--profile") {
                eprintln!("client: {:.1} ms from its start to the reply", started.elapsed().as_secs_f64() * 1000.0);
            }
            eprint!("{}", reply["stderr"].as_str().unwrap_or_default());
            print!("{}", reply["stdout"].as_str().unwrap_or_default());
            reply["code"].as_i64().unwrap_or(1) as i32
        }
        Err(error) => {
            eprintln!("ree: no service ({error}); running here");
            super::size_pool();
            command::cli(&args, &host, record)
        }
    }
}

/// A job comes in a burst when one with this build and environment came within [`BURST`] before it
/// (the services folder's `<build>-<env>.last`, touched by every client): only then is a service
/// started. A lone job runs in its client: a warm service saved it nothing measurable (the core/slam
/// sim: 0.63 s and 1.31 core-s here, 0.75 s and 1.22 core-s through a warm service, 2026-11-02), and
/// starting one cost ~0.4 s and ~0.4 core-s more and left ~400 MB held until it exited. The jobs of a
/// burst (a batch, @align passes, parallel lanes) share the service and its prepared runs.
const BURST: Duration = Duration::from_secs(10);

fn burst(state_dir: &Path, build: &str, env: &str) -> bool {
    let last = services_dir(state_dir).join(format!("{build}-{env}.last"));
    let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let previous = std::fs::read_to_string(&last).ok().and_then(|text| text.trim().parse::<u128>().ok());
    if std::fs::create_dir_all(services_dir(state_dir)).and_then(|()| std::fs::write(&last, now.to_string())).is_err() {
        return true;
    }
    previous.is_some_and(|previous| now.saturating_sub(previous) <= BURST.as_millis())
}

/// `ree serve --warm [engine options]`: this build's service for this environment (started when
/// missing) reads in what a first recording of the project reads (record::run::warm); prints its
/// JSON summary. The exit code: 0 when warm, 1 when there is no service or the warm-up failed.
pub fn warm(args: &[String]) -> i32 {
    let Some(state_dir) = super::default_state_dir("ree serve --warm") else { return 2 };
    // the service runs in its state dir, so a standalone warm-up carries the current folder as its project
    let args = if crate::is_standalone_binary() { named_project(args.to_vec()) } else { args.to_vec() };
    let body = json!({ "command": "warm", "args": absolute_paths(&args) }).to_string();
    match ensure(&state_dir).and_then(|port| post(port, body.as_bytes())) {
        Ok(reply) => {
            eprint!("{}", reply["stderr"].as_str().unwrap_or_default());
            print!("{}", reply["stdout"].as_str().unwrap_or_default());
            reply["code"].as_i64().unwrap_or(1) as i32
        }
        Err(error) => {
            eprintln!("ree serve --warm: no service ({error})");
            1
        }
    }
}

/// The arguments with the client's current folder added as `--project`, because the service runs in its state dir and
/// cannot tell which folder the run's relative local paths belong to. A standalone run with no project and no roots
/// gets it, whatever `--repo` names. A host, or a binary in a checkout, only gets it for a run of mounts alone.
pub fn named_project(mut args: Vec<String>) -> Vec<String> {
    let mut standalone_without_project = false;
    if crate::is_standalone_binary() {
        // Decide from the parsed arguments, as a direct run does: `--roots ""` and `--roots ";;"` parse as no roots, and a
        // repeated `--roots` keeps its last value. Arguments that do not parse go to the service as they are, which
        // reports the error.
        let Ok(parsed) = launch::parse(&args) else { return args };
        standalone_without_project = parsed.project.is_none() && parsed.roots.is_empty();
    }
    // The mounts-only check looks at the argument words only, so an empty `--roots` value counts as a project
    let has = |key: &str| args.iter().any(|arg| arg == key);
    let host_mounts_without_project = !has("--project") && !has("--roots") && (has("--mount") || has("--instance"));
    if standalone_without_project || host_mounts_without_project {
        if let Ok(dir) = std::env::current_dir() {
            args.push("--project".into());
            args.push(dir.to_string_lossy().into_owned());
        }
    }
    args
}

/// The arguments with every path the service reads or writes absolute (it runs elsewhere).
pub fn absolute_paths(args: &[String]) -> Vec<String> {
    let absolute = |path: &str| launch::absolute(path);
    let mut result = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        let key = args[index].as_str();
        result.push(args[index].clone());
        let Some(value) = args.get(index + 1) else { break };
        let converted = match key {
            "--harness" | "--out" | "--dir" | "--dump-scene" | "--project" | "--snapshot" | "--snapshot-dir" | "--repo" => Some(absolute(value)),
            // the service runs in another folder, so a standalone client's relative file name must name the client's file;
            // a host keeps forwarding it unchanged
            "--frame-times" if crate::is_standalone_binary() => Some(absolute(value)),
            "--context" if !value.trim_start().starts_with('{') => Some(absolute(value)),
            "--roots" | "--animations" => Some(value.split(';').filter(|dir| !dir.is_empty()).map(absolute).collect::<Vec<_>>().join(";")),
            "--mount" => Some(match value.split_once('=') {
                Some((at, path)) => format!("{at}={}", absolute(path)),
                None => value.clone(),
            }),
            _ => None,
        };
        if let Some(converted) = converted {
            result.push(converted);
            index += 2;
        } else {
            index += 1;
        }
    }
    result
}

/// The service a record names, when its process still runs a ree binary. A record outlives a service
/// that did not exit by itself (a crash, a kill, a reboot), and connecting to a port nothing listens
/// on takes Windows ~0.5 s per attempt.
fn read_info(path: &Path) -> Option<ServiceInfo> {
    let info: ServiceInfo = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    alive(info.pid).then_some(info)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(process: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn QueryFullProcessImageNameW(process: *mut std::ffi::c_void, flags: u32, name: *mut u16, size: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let mut code = 0u32;
    let running = unsafe { GetExitCodeProcess(process, &mut code) } != 0 && code == STILL_ACTIVE;
    // (a pid another process has since: its image is not a ree binary)
    let mut name = [0u16; 1024];
    let mut size = name.len() as u32;
    let image = (unsafe { QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut size) } != 0).then(|| String::from_utf16_lossy(&name[..size as usize]));
    unsafe { CloseHandle(process) };
    running && image.is_some_and(|image| Path::new(&image).file_name().is_some_and(|file| file.to_string_lossy().to_ascii_lowercase().starts_with("ree")))
}

#[cfg(not(windows))]
fn alive(_pid: u32) -> bool {
    true
}

/// Every service recorded for the state dir, running or not.
fn recorded(state_dir: &Path) -> Vec<ServiceInfo> {
    let Ok(entries) = std::fs::read_dir(services_dir(state_dir)) else { return Vec::new() };
    entries.flatten().filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json")).filter_map(|entry| read_info(&entry.path())).collect()
}

fn health(port: u16) -> Option<serde_json::Value> {
    let (status, body) = http::request(port, "GET", "/health", b"", Duration::from_secs(3)).ok()?;
    (status == 200).then(|| serde_json::from_slice(&body).ok()).flatten()
}

fn ready(info: &ServiceInfo, build: &str, env: &str) -> bool {
    info.build == build && info.env == env && health(info.port).is_some_and(|health| !health["shuttingDown"].as_bool().unwrap_or(false))
}

/// The running services of the state dir with their health.
pub fn status(state_dir: &Path) -> Vec<(ServiceInfo, serde_json::Value)> {
    recorded(state_dir).into_iter().filter_map(|info| Some((info.clone(), health(info.port)?))).collect()
}

/// Asks every service of the state dir to exit once its jobs finish; how many were asked.
pub fn stop(state_dir: &Path) -> usize {
    recorded(state_dir).iter().filter(|info| http::request(info.port, "POST", "/shutdown", b"{}", Duration::from_secs(3)).is_ok()).count()
}

/// The port of the service running this build with this environment, started if needed.
pub fn ensure(state_dir: &Path) -> Result<u16, String> {
    let (build, env) = (build_id(), env_digest());
    let record = record_path(state_dir, &build, &env);
    if let Some(info) = read_info(&record).filter(|info| ready(info, &build, &env)) {
        return Ok(info.port);
    }
    std::fs::create_dir_all(services_dir(state_dir)).map_err(|e| e.to_string())?;
    let lock = services_dir(state_dir).join(format!("{build}-{env}.lock"));
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(_) => break,
            Err(_) => {
                // another client is starting it: wait for theirs, or take over a stale lock
                if let Some(info) = read_info(&record).filter(|info| ready(info, &build, &env)) {
                    return Ok(info.port);
                }
                let stale = std::fs::metadata(&lock).and_then(|meta| meta.modified()).map(|time| SystemTime::now().duration_since(time).unwrap_or_default() > START_TIMEOUT).unwrap_or(true);
                if stale {
                    let _ = std::fs::remove_file(&lock);
                    continue;
                }
                if Instant::now() > deadline {
                    return Err("timed out waiting for another process to start the service".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    let result = match read_info(&record).filter(|info| ready(info, &build, &env)) {
        Some(info) => Ok(info.port),
        None => start(state_dir, &build, &env),
    };
    let _ = std::fs::remove_file(&lock);
    result
}

/// The service runs from a copy of the binary (`<state dir>/bin/ree-<build>.exe`), so the
/// original can be rebuilt while it runs; copies of other builds are removed once not running.
fn service_binary(state_dir: &Path, build: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("locating ree: {e}"))?;
    let directory = state_dir.join("bin");
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let extension = exe.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
    let name = format!("ree-{build}{extension}");
    let target = directory.join(&name);
    if !target.exists() {
        let temporary = directory.join(format!("{name}.{}.tmp", std::process::id()));
        std::fs::copy(&exe, &temporary).map_err(|e| format!("copying the service binary: {e}"))?;
        if std::fs::rename(&temporary, &target).is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
    }
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file != name && file.starts_with("ree-") {
                // fails while that build still runs; it goes on a later start
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(target)
}

fn source() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe).display().to_string())
}

fn start(state_dir: &Path, build: &str, env: &str) -> Result<u16, String> {
    // (the service runs in its state dir: a relative one is the caller's)
    let state_dir = &std::path::absolute(state_dir).map_err(|e| format!("{}: {e}", state_dir.display()))?;
    let exe = service_binary(state_dir, build)?;
    let log = OpenOptions::new().create(true).append(true).open(state_dir.join("serve.log")).map_err(|e| format!("opening serve.log: {e}"))?;
    let source = source();
    let mut command = std::process::Command::new(&exe);
    command.arg("serve").arg("--state-dir").arg(state_dir).arg("--build").arg(build).arg("--env").arg(env);
    if let Some(source) = &source {
        command.arg("--source").arg(source);
    }
    // started in its state dir: a process holds the folder it runs in, and the caller's may be a worktree about to be removed
    command.current_dir(state_dir);
    if crate::is_standalone_binary() {
        // exactly the environment the service key was hashed from
        command.env_clear().envs(launch::worker_environment());
    } else {
        command.envs(launch::path_variables());
    }
    command.stdin(std::process::Stdio::null()).stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
    let child = spawn_detached(&mut command).map_err(|e| format!("starting the service: {e}"))?;
    let pid = child.id();
    let deadline = Instant::now() + START_TIMEOUT;
    let record = record_path(state_dir, build, env);
    while Instant::now() < deadline {
        if let Some(info) = read_info(&record).filter(|info| info.pid == pid && health(info.port).is_some()) {
            // earlier builds of the same executable are superseded: they finish and exit
            for old in recorded(state_dir) {
                if old.build != build && old.source.is_some() && old.source == source {
                    let _ = http::request(old.port, "POST", "/shutdown", b"{}", Duration::from_secs(3));
                }
            }
            return Ok(info.port);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("the service did not come up (see {})", state_dir.join("serve.log").display()))
}

#[cfg(windows)]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE
    const STANDARD: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32];
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    // The child would otherwise inherit this process's stdout / stderr: a caller reading them
    // through a pipe (a preview tool, a shell) would never see them close while the
    // service lives.
    let handles: Vec<_> = STANDARD.iter().map(|&which| unsafe { GetStdHandle(which) }).filter(|handle| !handle.is_null() && *handle as isize != -1).collect();
    for &handle in &handles {
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
    }
    // Outlive the caller's job object (some shells kill their process trees) when allowed.
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
    let child = command.spawn().or_else(|_| {
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        command.spawn()
    });
    for &handle in &handles {
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    }
    child
}

#[cfg(not(windows))]
fn spawn_detached(command: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    command.spawn()
}

fn post(port: u16, body: &[u8]) -> Result<serde_json::Value, String> {
    let (status, response) = http::request(port, "POST", "/job", body, Duration::from_secs(3600))?;
    let value: serde_json::Value = serde_json::from_slice(&response).map_err(|e| format!("service reply: {e}"))?;
    if status != 200 {
        return Err(value["error"].as_str().unwrap_or("the job failed").to_string());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record whose process is gone, or whose pid another program has now, names no service.
    #[test]
    fn a_dead_or_foreign_pid_is_no_service() {
        assert!(!alive(std::process::id()), "this test binary is not a ree binary");
        let mut child = std::process::Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!alive(pid), "an exited process");
    }

    /// A lone job runs in its client; one within [`BURST`] of the last (this build and environment)
    /// starts a service.
    #[test]
    fn only_a_burst_of_jobs_starts_a_service() {
        let state = std::env::temp_dir().join(format!("ree-burst-{}-{}", std::process::id(), SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        assert!(!burst(&state, "b", "e"), "the first job");
        assert!(burst(&state, "b", "e"), "the next one at once");
        assert!(!burst(&state, "b", "other"), "another environment's first");
        let old = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() - BURST.as_millis() - 1000;
        std::fs::write(services_dir(&state).join("b-e.last"), old.to_string()).unwrap();
        assert!(!burst(&state, "b", "e"), "a job long after the last");
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn paths_are_made_absolute_and_other_arguments_kept() {
        let args: Vec<String> = ["--harness", "h.luau", "--seconds", "2", "--context", "{\"A\":1}", "--roots", "a;b", "--mount", "ReplicatedStorage/X=dir", "--verbose", "--animations", "anims", "--out", "o"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let converted = absolute_paths(&args);
        let at = |key: &str| converted[converted.iter().position(|a| a == key).unwrap() + 1].clone();
        assert!(Path::new(&at("--harness")).is_absolute());
        assert!(Path::new(&at("--out")).is_absolute());
        assert_eq!(at("--seconds"), "2");
        assert_eq!(at("--context"), "{\"A\":1}");
        assert!(at("--roots").split(';').all(|root| Path::new(root).is_absolute()));
        assert!(Path::new(&at("--animations")).is_absolute());
        assert!(at("--mount").starts_with("ReplicatedStorage/X=") && Path::new(at("--mount").split_once('=').unwrap().1).is_absolute());
        assert!(converted.contains(&"--verbose".to_string()));
        assert_eq!(converted.len(), args.len());
    }

    /// A run of mounts alone reads its content's relative local paths from its client's directory,
    /// which the service, in its state dir, is told as `--project`; one naming a project or roots is
    /// sent as it is
    #[test]
    fn a_run_of_mounts_alone_names_its_project() {
        let words = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let here = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        let mounted = named_project(words(&["--harness", "h.luau", "--mount", "ReplicatedStorage/X=x"]));
        assert_eq!(mounted[mounted.len() - 2..], [String::from("--project"), here]);
        for kept in [&["--harness", "h.luau"][..], &["--mount", "A/B=b", "--project", "p"], &["--instance", "A=A", "--roots", "r"]] {
            assert_eq!(named_project(words(kept)), words(kept), "{kept:?}");
        }
    }

    /// Hosts keep the original presence checks, including an empty roots value.
    #[test]
    fn a_host_keeps_empty_roots_as_named() {
        let words = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for roots in [&["--roots", ""][..], &["--roots", ";;"], &["--roots", "r", "--roots", ""]] {
            let args = words(&[&["--mount", "A/B=b"][..], roots].concat());
            let named = named_project(args.clone());
            assert_eq!(named, args, "{roots:?}");
        }
    }
}

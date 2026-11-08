//! A `ree run` cannot outlive its job (src/watchdog.rs): the wall-clock cap, the stop after repeated script timeouts and
//! the exit when the parent process is gone; and what a script exhausts (the held values of src/lua/refs.rs, the run's
//! stack) is its error, not the end of the run; and a profile it cannot take (a kind the profiling suite does not know)
//! stops a run before it starts, while the hardware counters' readings leave a recording as it was; through the built
//! `ree`.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("ree-guards-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(root.join("empty")).unwrap();
    root
}

fn harness(root: &Path, source: &str) -> String {
    let file = root.join("Guard.luau");
    std::fs::write(&file, source).unwrap();
    file.to_string_lossy().into_owned()
}

fn ree(root: &Path, harness: &str, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ree"));
    command.args(["run", "--harness", harness, "--roots", &root.join("empty").to_string_lossy()]).args(extra);
    for name in ["REE_SCRIPT_TIMEOUT", "REE_MAX_WALL_SECONDS", "REE_MAX_SCRIPT_TIMEOUTS", "REE_PARENT_WATCH"] {
        command.env_remove(name);
    }
    command
}

#[test]
fn remote_function_invoke_server_uses_loopback_handler() {
    let root = scratch("remote-function-loopback");
    let remote = harness(
        &root,
        "return function()\n            local players = game:GetService('Players')\n            local remote = Instance.new('RemoteFunction')\n            remote.OnServerInvoke = function(player, value)\n                assert(player == players.LocalPlayer)\n                return value + 1, 'server'\n            end\n            local value, source = remote:InvokeServer(41)\n            print('remote result', value, source)\n            print('@done')\n        end",
    );
    let done = ree(&root, &remote, &["--seconds", "1", "--loopback"]).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.status.code(), Some(0), "{stderr}");
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).expect("the summary on stdout");
    assert_eq!(summary["done"], true, "{summary}");
    assert!(summary["output"].as_array().unwrap().iter().any(|entry| entry["message"] == "remote result 42 server"), "{summary}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn remote_function_invoke_server_waits_for_late_loopback_handler() {
    let root = scratch("remote-function-late-loopback");
    let remote = harness(
        &root,
        "return function()\n            local remote = Instance.new('RemoteFunction')\n            task.delay(0.1, function() remote.OnServerInvoke = function(_, value) return value + 1 end end)\n            print('late result', remote:InvokeServer(41))\n            print('@done')\n        end",
    );
    let done = ree(&root, &remote, &["--seconds", "1", "--loopback"]).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.status.code(), Some(0), "{stderr}");
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).expect("the summary on stdout");
    assert_eq!(summary["done"], true, "{summary}");
    assert!(summary["output"].as_array().unwrap().iter().any(|entry| entry["message"] == "late result 42"), "{summary}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn user_game_settings_and_player_gui_have_offline_defaults() {
    let root = scratch("client-settings");
    let client = harness(
        &root,
        "return function()\n            local settings = UserSettings():GetService('UserGameSettings')\n            assert(settings:GetCameraYInvertValue() == 1)\n            local playerGui = Instance.new('PlayerGui')\n            local objects = playerGui:GetGuiObjectsAtPosition(0, 0)\n            assert(type(objects) == 'table' and #objects == 0)\n            print('@done')\n        end",
    );
    let done = ree(&root, &client, &["--seconds", "1"]).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.status.code(), Some(0), "{stderr}");
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).expect("the summary on stdout");
    assert_eq!(summary["done"], true, "{summary}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_run_past_its_wall_clock_cap_exits_124() {
    let root = scratch("cap");
    let spin = harness(&root, "return function() while true do end end");
    let started = Instant::now();
    let done = ree(&root, &spin, &["--seconds", "1", "--script-timeout", "0", "--max-wall-seconds", "3"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert_eq!(done.status.code(), Some(124), "{stderr}");
    assert!(stderr.contains("wall-clock cap reached after 3 s"), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(30));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn repeated_script_timeouts_stop_the_run_with_126_and_its_output() {
    let root = scratch("timeouts");
    let spin = harness(&root, "return function() game:GetService('RunService').Heartbeat:Connect(function() while true do end end) end");
    let done = ree(&root, &spin, &["--seconds", "5", "--fps", "10", "--script-timeout", "0.02", "--max-script-timeouts", "2"]).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.status.code(), Some(126), "{stderr}");
    assert!(stderr.contains("ree: script timeout 2 at"), "{stderr}");
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).expect("the summary on stdout");
    assert_eq!(summary["status"], "error");
    assert!(summary["failure"].as_str().unwrap().contains("2 script timeouts: the run stops"), "{summary}");
    assert!(summary["frames"].as_u64().unwrap() < 10, "{summary}");
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(windows)]
#[test]
fn a_run_whose_parent_is_gone_exits() {
    // cmd starts ree in the background and exits at once: ree's parent is gone
    let root = scratch("parent");
    let wait = harness(&root, "return function() while true do task.wait() end end");
    let log = root.join("ree.log");
    let line = format!(
        "start \"\" /b \"{}\" run --harness \"{wait}\" --roots \"{}\" --seconds 1000 --max-wall-seconds 60 > \"{}\" 2>&1",
        env!("CARGO_BIN_EXE_ree"),
        root.join("empty").display(),
        log.display()
    );
    // (cmd reads its own quoting: the line goes as it is)
    use std::os::windows::process::CommandExt;
    let status = Command::new("cmd").raw_arg(format!("/d /s /c \"{line}\"")).status().unwrap();
    assert!(status.success());
    let started = Instant::now();
    let mut text = String::new();
    while started.elapsed() < Duration::from_secs(45) {
        text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("is gone; exiting") {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(text.contains("its parent process (pid") && text.contains("is gone; exiting"), "{text}");
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_runaway_that_fills_the_reference_stack_errors_in_its_script_and_the_run_goes_on() {
    // A Heartbeat handler runs in a resumption no Luau pcall wraps: there mlua's panic for a full reference stack ended
    // the run with exit 101 and no summary (a loop that queued a task.defer per hit). A BindableEvent fire
    // with a table argument holds a slot until its handler runs, as a deferred thread does, without a thread's memory.
    let root = scratch("refs");
    let runaway = harness(
        &root,
        "return function()
            local Event = Instance.new('BindableEvent')
            local Handled = 0
            Event.Event:Connect(function() Handled += 1 end)
            local Fired = false
            game:GetService('RunService').Heartbeat:Connect(function()
                if Fired then return end
                Fired = true
                for _ = 1, 2000000 do Event:Fire({}) end
            end)
            task.wait(1)
            print('handled', Handled > 800000)
            print('@done')
        end",
    );
    let done = ree(&root, &runaway, &["--seconds", "3"]).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).expect("the summary on stdout");
    assert_eq!(summary["done"], true, "{summary}");
    let output = summary["output"].as_array().unwrap();
    let refused = output.iter().find(|entry| entry["message"].as_str().unwrap().contains("Fire: too many threads and values held for scripts")).expect("the refusal");
    assert_eq!(refused["kind"], "error");
    assert!(refused["trace"].as_str().unwrap().contains("Guard"), "{refused}");
    assert!(output.iter().any(|entry| entry["message"] == "handled true"), "{summary}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_deep_instance_tree_does_not_overflow_a_runs_stack() {
    // GetDescendants, Clone, parenting and Destroy recurse along the tree: on the main thread's 1 MiB they overflowed it
    // on a 3,000-deep tree, where the service's workers (64 MiB) ran the same job
    let root = scratch("deep");
    let deep = harness(
        &root,
        "return function()
            local Root = Instance.new('Folder')
            local At = Root
            for _ = 1, 3000 do
                local Child = Instance.new('Folder')
                Child.Parent = At
                At = Child
            end
            print(#Root:GetDescendants(), Root:Clone() ~= nil)
            Root.Parent = workspace
            Root:Destroy()
            print('@done')
        end",
    );
    let done = ree(&root, &deep, &["--seconds", "1"]).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&done.stdout), String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.status.code(), Some(0), "{stderr}");
    let summary: serde_json::Value = serde_json::from_str(stdout.trim()).expect("the summary on stdout");
    assert!(summary["output"].as_array().unwrap().iter().any(|entry| entry["message"] == "3000 true"), "{summary}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_profile_kind_ree_does_not_know_stops_a_recording_before_it_starts() {
    // it used to be taken for `--profile` alone: the recording ran, wrote no profile and exited 0
    let root = scratch("profile-kind");
    let said = harness(&root, "return function() print('ran') end");
    let out = root.join("out");
    let mut record = Command::new(env!("CARGO_BIN_EXE_ree"));
    record.args(["record", "--harness", &said, "--roots", &root.join("empty").to_string_lossy(), "--out", &out.to_string_lossy(), "--profile", "memroy"]).current_dir(&root);
    for name in ["REE_PROFILE", "REE_PROFILE_OUT", "REE_SCRIPT_TIMEOUT", "REE_MAX_WALL_SECONDS", "REE_MAX_SCRIPT_TIMEOUTS", "REE_PARENT_WATCH"] {
        record.env_remove(name);
    }
    let done = record.output().unwrap();
    let stderr = String::from_utf8_lossy(&done.stderr);
    assert_eq!(done.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("no kind \"memroy\" (kinds: work, caches, alloc, heap, timeline, cpu, hw, phases, time, boundary, all)"), "{stderr}");
    assert!(done.stdout.is_empty(), "nothing ran: {}", String::from_utf8_lossy(&done.stdout));
    assert!(!out.exists(), "no recording, no profile");
    let _ = std::fs::remove_dir_all(&root);
}

/// The hardware counters change nothing a recording holds: with the mock counter source (REE_HW_COUNTERS=mock: the
/// readings at every frame, phase mark, engine step and safepoint sample are taken, the thread's cycle count standing in
/// for the CPU's counters), a frozen-clock recording is byte for byte the unprofiled one, and the profile has its counts
#[test]
fn hardware_counters_leave_a_recording_as_it_was() {
    let root = scratch("hw-recording");
    let busy = harness(&root, "return function(Context)\n\tlocal RunService = game:GetService(\"RunService\")\n\tlocal parts = {}\n\tfor i = 1, 200 do\n\t\tlocal part = Instance.new(\"Part\")\n\t\tpart.Position = Vector3.new(i, 0, 0)\n\t\tpart.Parent = workspace\n\t\tparts[i] = part\n\tend\n\tContext.ProfilePhase(\"steady\")\n\tRunService.Heartbeat:Connect(function(dt)\n\t\tfor i, part in parts do\n\t\t\tpart.Position += Vector3.new(0, dt * (i % 7), 0)\n\t\tend\n\t\tlocal x = 0\n\t\tfor i = 1, 20000 do\n\t\t\tx += i % 13\n\t\tend\n\tend)\nend\n");
    let record = |name: &str, profile: Option<&Path>| -> PathBuf {
        let out = root.join(name);
        let mut command = Command::new(env!("CARGO_BIN_EXE_ree"));
        command.args(["record", "--harness", &busy, "--roots", &root.join("empty").to_string_lossy(), "--out", &out.to_string_lossy(), "--seconds", "1"]).current_dir(&root);
        for name in ["REE_PROFILE", "REE_PROFILE_OUT", "REE_PROFILE_COUNTERS", "REE_SCRIPT_TIMEOUT", "REE_MAX_WALL_SECONDS", "REE_MAX_SCRIPT_TIMEOUTS", "REE_PARENT_WATCH"] {
            command.env_remove(name);
        }
        command.env("REE_FROZEN_CLOCK", "1").env("RUR_EPOCH", "1790460000").env("REE_HW_COUNTERS", "mock");
        if let Some(profile) = profile {
            command.args(["--profile", "hw,timeline", "--profile-out", &profile.to_string_lossy()]);
        }
        let done = command.output().unwrap();
        assert_eq!(done.status.code(), Some(0), "{}", String::from_utf8_lossy(&done.stderr));
        out
    };
    let profile = root.join("profile");
    let (plain, counted) = (record("plain", None), record("counted", Some(&profile)));
    let mut files: Vec<String> = std::fs::read_dir(&plain).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    files.sort();
    assert!(files.iter().any(|file| file == "playback.json") && files.iter().any(|file| file == "scene.rbxm"), "{files:?}");
    for file in &files {
        let (a, b) = (std::fs::read(plain.join(file)).unwrap(), std::fs::read(counted.join(file)).unwrap());
        if file == "summary.json" {
            // (the summary names the folder it was written to)
            let strip = |bytes: &[u8]| -> serde_json::Value {
                let mut summary: serde_json::Value = serde_json::from_slice(bytes).unwrap();
                summary.as_object_mut().unwrap().remove("out");
                summary
            };
            assert_eq!(strip(&a), strip(&b), "summary.json");
        } else {
            assert!(a == b, "{file} differs with --profile hw");
        }
    }
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(profile.join("profile.json")).unwrap()).unwrap();
    let counters = &json["counters"];
    assert_eq!(counters["source"], "mock", "{counters}");
    assert!(counters["run"]["counts"]["cycles"].as_u64().unwrap() > 0, "{counters}");
    assert!(counters["readings"]["samples"].as_u64().unwrap() > 0, "{}", counters["readings"]);
    assert_eq!(counters["readings"]["matched"], counters["readings"]["count"], "{}", counters["readings"]);
    // (a row a frame: as many as the timeline's, which the same frame hook takes)
    let frames = counters["frames"]["rows"].as_array().unwrap().len();
    assert!(frames > 0 && frames == json["timeline"]["rows"].as_array().unwrap().len(), "{frames} frames counted");
    let _ = std::fs::remove_dir_all(&root);
}

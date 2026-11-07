//! Differential tests against the real engine (tools/parity, README.md there): every probe with a captured expected
//! output runs through the built `ree`, and every expected line it does not print must be listed in known.json as a
//! known difference. A fix that makes a known line match passes too; `node tools/parity/run.mjs` says which entries
//! to remove. The server's set (expected/server, known-server.json) runs the same way with `--run-context server`.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
};

fn parity_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tools").join("parity")
}

/// The text both engines run (tools/parity/run.mjs `body`).
fn body(prelude: &str, probe: &str) -> String {
    format!(
        "{prelude}\nP.base = debug.info(1, \"l\") local __ParityOk, __ParityError = pcall(function()\n{probe}\nend)\nif not __ParityOk then\n\tprint(\"probe-error \" .. P.err(__ParityError))\nend\n"
    )
}

fn expected_lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n").lines().filter(|line| !line.starts_with("# ")).map(str::to_owned).collect()
}

/// Expected lines missing from `actual`, in order (the lines a longest common subsequence leaves out).
fn missing(expected: &[String], actual: &[String]) -> Vec<String> {
    let (n, m) = (expected.len(), actual.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if expected[i] == actual[j] { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n {
        if j < m && expected[i] == actual[j] {
            i += 1;
            j += 1;
        } else if j < m && table[i][j + 1] >= table[i + 1][j] {
            j += 1;
        } else {
            out.push(expected[i].clone());
            i += 1;
        }
    }
    out
}

/// The probe's print collects its lines and the harness prints them when the probe has ended, as the real engine's test
/// runner does, so they never reach LogService.MessageOut (tools/parity/run.mjs HARNESS_PRINT and HARNESS_FLUSH: the same
/// text).
const HARNESS_PRINT: &str = "local __ParityLines, __ParityRealPrint = {}, print\nlocal function print(...)\n\tlocal Parts = table.pack(...)\n\tfor Index = 1, Parts.n do\n\t\tParts[Index] = tostring(Parts[Index])\n\tend\n\ttable.insert(__ParityLines, table.concat(Parts, \" \", 1, Parts.n))\nend";
const HARNESS_FLUSH: &str = "for _, Line in __ParityLines do\n\t__ParityRealPrint(Line)\nend\n__ParityRealPrint(\"@done\")";

fn run_probe(scratch: &Path, name: &str, source: &str, server: bool) -> Vec<String> {
    let harness = scratch.join(format!("{name}.luau"));
    std::fs::write(&harness, format!("return function(Context)\n{HARNESS_PRINT}\n{source}\n{HARNESS_FLUSH}\nend\n")).unwrap();
    let empty = scratch.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let context: &[&str] = if server { &["--run-context", "server"] } else { &[] };
    let done = Command::new(env!("CARGO_BIN_EXE_ree"))
        .args(["run", "--harness", &harness.to_string_lossy(), "--roots", &empty.to_string_lossy(), "--seconds", "120"])
        .args(context)
        .env_remove("REE_FROZEN_CLOCK")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&done.stdout);
    let summary: serde_json::Value = serde_json::from_str(stdout.trim().lines().last().unwrap_or("{}"))
        .unwrap_or_else(|_| panic!("{name}: no summary: {}", String::from_utf8_lossy(&done.stderr)));
    assert_eq!(summary["done"], true, "{name}: the probe did not finish: {summary}");
    let mut lines = Vec::new();
    for entry in summary["output"].as_array().unwrap() {
        let message = entry["message"].as_str().unwrap_or_default();
        if entry["kind"] == "print" && message != "@done" {
            lines.extend(message.split('\n').map(str::to_owned));
        }
    }
    lines
}

#[test]
fn probes_print_what_the_real_engine_printed() {
    check(false);
}

/// The probes run as a server's script (a Studio 0.744 play session's server: capture_sim.py), ree with --run-context server
#[test]
fn probes_print_on_a_server_what_a_real_server_printed() {
    check(true);
}

fn check(server: bool) {
    let dir = parity_dir();
    let prelude = std::fs::read_to_string(dir.join("prelude.luau")).unwrap();
    let known_file = if server { "known-server.json" } else { "known.json" };
    let known: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(known_file)).unwrap()).unwrap();
    let expected_dir = if server { dir.join("expected").join("server") } else { dir.join("expected") };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let side = if server { "server" } else { "client" };
    let scratch = std::env::temp_dir().join(format!("ree-parity-{side}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(dir.join("probes"))
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.strip_suffix(".luau").map(str::to_owned))
        .collect();
    names.sort();
    let mut failures = Vec::new();
    let mut checked = 0;
    for name in &names {
        let Ok(expected) = std::fs::read_to_string(expected_dir.join(format!("{name}.txt"))) else { continue };
        let expected = expected_lines(&expected);
        let probe = std::fs::read_to_string(dir.join("probes").join(format!("{name}.luau"))).unwrap();
        let actual = run_probe(&scratch, name, &body(&prelude, &probe), server);
        let allowed: HashSet<&str> =
            known[name.as_str()].as_array().map(|list| list.iter().filter_map(|entry| entry["line"].as_str()).collect()).unwrap_or_default();
        for line in missing(&expected, &actual) {
            if !allowed.iter().any(|prefix| line.starts_with(prefix)) {
                failures.push(format!("{name}: the real engine printed `{line}`"));
            }
        }
        checked += 1;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(checked > 0, "no probe has an expected output");
    let command = if server { "node tools/parity/run.mjs --server --diff" } else { "node tools/parity/run.mjs --diff" };
    assert!(failures.is_empty(), "{} new differences from the real engine ({command}):\n{}", failures.len(), failures.join("\n"));
}

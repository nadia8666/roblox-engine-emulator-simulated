#!/usr/bin/env node
// Differential tests against the real engine: every probe in tools/parity/probes runs in ree and its printed lines are
// compared with what the same probe printed in a Roblox client (tools/parity/expected). README.md says how to read the
// table and how to capture a new expected set after a Roblox update.
//
//   node tools/parity/run.mjs [probe ...] [--ree <exe>] [--diff] [--json <file>] [--write-known]
//   node tools/parity/run.mjs --capture [probe ...]          # run the probes in the real engine (REE_PARITY_CAPTURE)
//   node tools/parity/run.mjs ui_signals --rur <checkout>    # through rur, a host that adds a GUI layout
//   node tools/parity/run.mjs --server [...]                 # the probes as a server's script: expected/server and
//                                                            # known-server.json, ree with --run-context server
//   node tools/parity/run.mjs --loopback [...]               # the paired probes (probes/loopback): a server and its client
//                                                            # in one play session, expected/loopback and known-loopback.json,
//                                                            # ree with --run-context loopback
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..");
const PROBES = path.join(HERE, "probes");
const EXPECTED = path.join(HERE, "expected");
const KNOWN = path.join(HERE, "known.json");
// The server's set: each probe run as a server Script (capture_sim.py), and ree with --run-context server
const EXPECTED_SERVER = path.join(EXPECTED, "server");
const KNOWN_SERVER = path.join(HERE, "known-server.json");
// The paired set: each probe a server's half and a client's half of one play session (loopback.luau), ree with
// --run-context loopback
const PROBES_LOOPBACK = path.join(PROBES, "loopback");
const EXPECTED_LOOPBACK = path.join(EXPECTED, "loopback");
const KNOWN_LOOPBACK = path.join(HERE, "known-loopback.json");

export function probeNames() {
	return fs.readdirSync(PROBES).filter(name => name.endsWith(".luau")).map(name => name.slice(0, -5)).sort();
}

// The text both engines run: the prelude, then the probe. Line numbers in error messages are made relative to the
// prelude's first line (P.err), so the same probe gives the same text wherever it is wrapped.
export function body(name) {
	const prelude = fs.readFileSync(path.join(HERE, "prelude.luau"), "utf8");
	const probe = fs.readFileSync(path.join(PROBES, `${name}.luau`), "utf8");
	// a probe that fails prints one comparable line instead of ending the run differently in each engine
	// (P.base is the line above the probe's first, so "@<n>:" in an error is the probe's own line n)
	return `${prelude}\nP.base = debug.info(1, "l") local __ParityOk, __ParityError = pcall(function()\n${probe}\nend)\nif not __ParityOk then\n\tprint("probe-error " .. P.err(__ParityError))\nend\n`;
}

export function loopbackProbeNames() {
	if (!fs.existsSync(PROBES_LOOPBACK)) return [];
	return fs.readdirSync(PROBES_LOOPBACK).filter(name => name.endsWith(".luau")).map(name => name.slice(0, -5)).sort();
}

// One side of a paired probe (probes/loopback/<name>.luau: lines both sides run, then `--- server` and the server's lines,
// then `--- client` and the client's): the whole file with the other side's lines blanked, so an error's "@<n>:" is the
// file's own line n on either side. Its print collects the lines, and loopback.luau's __LoopbackFinish sends the server's
// to the client, which prints both sides' (`server: ...`, then `client: ...`) with the engine's print. tests/parity.rs
// builds the same text.
export function loopbackText(name, side) {
	const prelude = fs.readFileSync(path.join(HERE, "prelude.luau"), "utf8");
	const helpers = fs.readFileSync(path.join(HERE, "loopback.luau"), "utf8");
	const lines = fs.readFileSync(path.join(PROBES_LOOPBACK, `${name}.luau`), "utf8").replace(/\r\n/g, "\n").split("\n");
	let section = "both";
	const kept = lines.map(line => {
		const marker = line.match(/^--- (server|client)\s*$/);
		if (marker) {
			section = marker[1];
			return line;
		}
		return section === "both" || section === side ? line : "";
	});
	return `${LOOPBACK_COLLECT}\n${prelude}\n${helpers}\nP.base = debug.info(1, "l") local __ParityOk, __ParityError = pcall(function()\n${kept.join("\n")}\nend)\nif not __ParityOk then\n\tprint("probe-error " .. P.err(__ParityError))\nend\n__LoopbackFinish(__LoopbackLines, __LoopbackRealPrint)\n`;
}

// Each side's print collects its lines (loopback.luau prints them when both sides are done)
export const LOOPBACK_COLLECT = `local __LoopbackLines, __LoopbackRealPrint = {}, print
local function print(...)
	local Parts = table.pack(...)
	for Index = 1, Parts.n do
		Parts[Index] = tostring(Parts[Index])
	end
	table.insert(__LoopbackLines, table.concat(Parts, " ", 1, Parts.n))
end`;

// Header lines of an expected file start with "# ".
export function readExpected(name, server = false, loopback = false) {
	const file = path.join(loopback ? EXPECTED_LOOPBACK : server ? EXPECTED_SERVER : EXPECTED, `${name}.txt`);
	if (!fs.existsSync(file)) return null;
	const text = fs.readFileSync(file, "utf8").replace(/\r\n/g, "\n");
	const lines = text.split("\n");
	if (lines.at(-1) === "") lines.pop();
	return { header: lines.filter(line => line.startsWith("# ")), lines: lines.filter(line => !line.startsWith("# ")) };
}

// A probe's options, from a line "-- parity: identity=4 device=desktop rendered flag=FFlagEnableLoadModule" (capture_sim.py
// reads the same line; "flag" names one engine flag the capture turns on).
export function probeOptions(name) {
	const probe = fs.readFileSync(path.join(PROBES, `${name}.luau`), "utf8");
	const match = probe.match(/--\s*parity:([^\r\n]*)/);
	const options = {};
	for (const word of match ? match[1].trim().split(/\s+/).filter(Boolean) : []) {
		const [key, value] = word.split("=");
		options[key] = value ?? true;
	}
	return options;
}

export function readKnown(server = false, loopback = false) {
	const file = loopback ? KNOWN_LOOPBACK : server ? KNOWN_SERVER : KNOWN;
	if (!fs.existsSync(file)) return {};
	return JSON.parse(fs.readFileSync(file, "utf8"));
}

// Whether a probe runs on a server too: not one that needs a client's screen (a device or a renderer), nor one ree runs as a
// plugin's code ("context=edit")
export function serverProbe(name) {
	const options = probeOptions(name);
	return !options.device && !options.rendered && !options.context;
}

function defaultRee() {
	const exe = path.join(REPO, "target", "release", process.platform === "win32" ? "ree.exe" : "ree");
	return fs.existsSync(exe) ? exe : null;
}

// The probe's print collects its lines and the harness prints them when the probe has ended, as a real engine's test runner
// does (capture_sim.py: the job's own print is a collector), so the probe's lines never reach LogService.MessageOut, which
// a probe may be listening to. tests/parity.rs wraps the same way.
export const HARNESS_PRINT = `local __ParityLines, __ParityRealPrint = {}, print
local function print(...)
	local Parts = table.pack(...)
	for Index = 1, Parts.n do
		Parts[Index] = tostring(Parts[Index])
	end
	table.insert(__ParityLines, table.concat(Parts, " ", 1, Parts.n))
end`;
export const HARNESS_FLUSH = `for _, Line in __ParityLines do
	__ParityRealPrint(Line)
end
__ParityRealPrint("@done")`;

// One probe through `ree run`: its printed lines (the run's own `@done` dropped), the errors and warnings scripts and the
// engine reported (output, not failures: a real engine reports them too), and why the run failed if it did.
export function runInRee(name, ree, scratch, server = false) {
	const harness = path.join(scratch, `${name}.luau`);
	fs.writeFileSync(harness, `return function(Context)\n${HARNESS_PRINT}\n${body(name)}\n${HARNESS_FLUSH}\nend\n`);
	const empty = path.join(scratch, "empty");
	fs.mkdirSync(empty, { recursive: true });
	// (the server capture's place has ServerScriptService.LoadStringEnabled on; a probe's "flag=" names an engine flag it needs on;
	// "context=edit" runs it as a plugin's code, in ree's edit run context: a probe captured at identity 4, which has the
	// Plugin capability a game script lacks)
	const options = probeOptions(name);
	const context = server ? ["--run-context", "server", "--load-string"] : options.context === "edit" ? ["--mode", "edit", "--run-context", "edit"] : [];
	if (options.flag === "FFlagEnableLoadModule") context.push("--load-module");
	const result = spawnSync(ree, ["run", "--harness", harness, "--roots", empty, "--seconds", "120", ...context], {
		encoding: "utf8", maxBuffer: 256 * 1024 * 1024, windowsHide: true,
	});
	return readRun(result);
}

// A `ree run`'s printed lines (its `@done` dropped), the other output lines, and why it failed if it did
function readRun(result) {
	let summary;
	try {
		summary = JSON.parse(result.stdout.trim().split("\n").at(-1));
	} catch {
		return { lines: [], messages: [], errors: [`ree exited ${result.status}: ${(result.stderr || "").trim().split("\n").slice(-3).join(" / ")}`] };
	}
	const lines = [];
	const errors = [];
	const messages = [];
	for (const entry of summary.output || []) {
		if (entry.kind === "print" && entry.message !== "@done") lines.push(...String(entry.message).split("\n"));
		else if (entry.kind !== "print") messages.push(`${entry.side ? `${entry.side} ` : ""}${entry.kind}: ${String(entry.message).split("\n")[0]}`);
	}
	if (!summary.done) errors.push(`the probe did not finish (status ${summary.status}${summary.failure ? `: ${summary.failure}` : ""})`);
	return { lines, messages, errors };
}

// One paired probe through `ree run --run-context loopback`: the harness's Server and Client run the probe's two sides; the
// client prints both sides' lines (loopback.luau), then `@done`.
export function runInReeLoopback(name, ree, scratch) {
	const harness = path.join(scratch, `${name}.luau`);
	fs.writeFileSync(harness, `return {\n\tServer = function(Context)\n${loopbackText(name, "server")}\nend,\n\tClient = function(Context)\n${loopbackText(name, "client")}\n__LoopbackRealPrint("@done")\nend,\n}\n`);
	const empty = path.join(scratch, "empty");
	fs.mkdirSync(empty, { recursive: true });
	const result = spawnSync(ree, ["run", "--harness", harness, "--roots", empty, "--seconds", "120", "--run-context", "loopback"], {
		encoding: "utf8", maxBuffer: 256 * 1024 * 1024, windowsHide: true,
	});
	return readRun(result);
}

// One probe through rur (roblox-ui-renderer), which adds a GUI layout and a renderer to ree: `rur render` of the body as
// a fixture, in play mode on the probe's device, on the capture client's screen (1920x1080 at scale 1 under a 58 px
// top-bar inset, as a probe's coordinates line prints it). RUR_NATIVE_ENGINE picks a rur-native build, as for rur itself.
export function runInRur(name, checkout, scratch) {
	const fixture = path.join(scratch, `${name}.luau`);
	fs.writeFileSync(fixture, `return function(Target)\n${HARNESS_PRINT}\n${body(name)}\n${HARNESS_FLUSH}\nend\n`);
	const out = path.join(scratch, `${name}-rur`);
	const device = String(probeOptions(name).device || "desktop");
	const result = spawnSync(process.execPath, [path.join(checkout, "cli", "rur.mjs"), "render", "--fixture", fixture, "--mode", "play",
		"--device", device, "--topbar-inset", "58", "--viewport", "1920x1080", "--scale", "1", "--time", "5", "--out", out], {
		encoding: "utf8", cwd: checkout, maxBuffer: 256 * 1024 * 1024, windowsHide: true,
	});
	let scene;
	try {
		scene = JSON.parse(fs.readFileSync(path.join(out, "scene.json"), "utf8"));
	} catch {
		return { lines: [], messages: [], errors: [`rur exited ${result.status}: ${(result.stderr || "").trim().split("\n").slice(-3).join(" / ")}`] };
	}
	const lines = [];
	const messages = [];
	let done = false;
	for (const entry of scene.diagnostics?.output || []) {
		if (entry.kind === "print" && entry.message === "@done") done = true;
		else if (entry.kind === "print") lines.push(...String(entry.message).split("\n"));
		else messages.push(`${entry.kind}: ${String(entry.message).split("\n")[0]}`);
	}
	return { lines, messages, errors: done ? [] : ["the probe did not finish within rur's 5 seconds"] };
}

// A line diff (longest common subsequence), as [kind, expectedIndex, actualIndex] with kind " ", "-" or "+".
export function diffLines(expected, actual) {
	const n = expected.length, m = actual.length;
	const table = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
	for (let i = n - 1; i >= 0; i--) {
		for (let j = m - 1; j >= 0; j--) {
			table[i][j] = expected[i] === actual[j] ? table[i + 1][j + 1] + 1 : Math.max(table[i + 1][j], table[i][j + 1]);
		}
	}
	const out = [];
	let i = 0, j = 0;
	while (i < n || j < m) {
		if (i < n && j < m && expected[i] === actual[j]) {
			out.push([" ", i++, j++]);
		} else if (j < m && (i === n || table[i][j + 1] >= table[i + 1][j])) {
			out.push(["+", null, j++]);
		} else {
			out.push(["-", i++, null]);
		}
	}
	return out;
}

// Compare a probe's lines with the expected ones: every expected line ree did not print is a difference, unless
// known.json lists it (by the start of the expected line) as a known one.
export function compare(name, expected, actual, known) {
	const notes = known[name] || [];
	const diff = diffLines(expected, actual);
	const missing = diff.filter(([kind]) => kind === "-").map(([, i]) => expected[i]);
	const extra = diff.filter(([kind]) => kind === "+").map(([, , j]) => actual[j]);
	const knownLines = [];
	const unknownLines = [];
	for (const line of missing) {
		const note = notes.find(entry => line.startsWith(entry.line));
		(note ? knownLines : unknownLines).push(line);
	}
	const stale = notes.filter(entry => !missing.some(line => line.startsWith(entry.line))).map(entry => entry.line);
	return { total: expected.length, same: expected.length - missing.length, known: knownLines, unknown: unknownLines, extra, stale, diff };
}

function pad(text, width) {
	text = String(text);
	return text.length >= width ? text : text + " ".repeat(width - text.length);
}

function capture(names, server = false, loopback = false) {
	const command = process.env.REE_PARITY_CAPTURE;
	if (!command) {
		console.error("parity: --capture runs the probes in the real engine through the command REE_PARITY_CAPTURE names; README.md says what it is given and must write");
		process.exit(2);
	}
	const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "ree-parity-"));
	const input = path.join(scratch, "bodies.json");
	const output = path.join(scratch, "outputs.json");
	// (a paired probe gives each side's whole text: the server's runs as a server Script, the client's as the client's job)
	const entry = name => loopback ? { name, loopback: true, server: loopbackText(name, "server"), client: loopbackText(name, "client") }
		: server ? { name, body: body(name), server: true } : { name, body: body(name) };
	fs.writeFileSync(input, JSON.stringify(names.map(entry)));
	const result = spawnSync(`${command} "${input}" "${output}"`, { stdio: "inherit", shell: true, windowsHide: true, cwd: REPO });
	if (result.status !== 0 || !fs.existsSync(output)) {
		console.error(`parity: the capture command failed (exit ${result.status})`);
		process.exit(1);
	}
	const captured = JSON.parse(fs.readFileSync(output, "utf8"));
	const folder = loopback ? EXPECTED_LOOPBACK : server ? EXPECTED_SERVER : EXPECTED;
	fs.mkdirSync(folder, { recursive: true });
	for (const name of names) {
		const entry = captured.probes?.[name];
		if (!entry || !entry.ok) {
			console.error(`parity: ${name}: not captured (${entry ? entry.error : "missing"})`);
			continue;
		}
		const header = [`# engine: ${entry.engine || captured.engine || "unknown"}`, `# captured: ${captured.date || new Date().toISOString().slice(0, 10)}`];
		fs.writeFileSync(path.join(folder, `${name}.txt`), [...header, ...entry.lines].join("\n") + "\n");
		console.log(`parity: ${name}: ${entry.lines.length} lines`);
	}
	fs.rmSync(scratch, { recursive: true, force: true });
}

function main() {
	const args = process.argv.slice(2);
	const options = { ree: defaultRee(), rur: null, diff: false, json: null, capture: false, writeKnown: false, server: false, loopback: false, names: [] };
	for (let index = 0; index < args.length; index++) {
		const arg = args[index];
		if (arg === "--ree") options.ree = args[++index];
		else if (arg === "--rur") options.rur = args[++index];
		else if (arg === "--diff") options.diff = true;
		else if (arg === "--json") options.json = args[++index];
		else if (arg === "--capture") options.capture = true;
		else if (arg === "--write-known") options.writeKnown = true;
		else if (arg === "--server") options.server = true;
		else if (arg === "--loopback") options.loopback = true;
		else if (arg.startsWith("-")) throw new Error(`unknown option ${arg}`);
		else options.names.push(arg);
	}
	if (options.server && options.loopback) throw new Error("--loopback runs both sides; leave out --server");
	const names = options.names.length ? options.names : options.loopback ? loopbackProbeNames() : probeNames().filter(name => !options.server || serverProbe(name));
	if (options.capture) return capture(names, options.server, options.loopback);
	if (!options.ree && !options.rur) throw new Error("no ree: build it (cargo build --release) or pass --ree <exe>");
	if (options.rur && options.writeKnown) throw new Error("--write-known records ree's differences, not rur's");
	if (options.rur && options.server) throw new Error("--server runs ree as a server; rur hosts a client");
	if (options.rur && options.loopback) throw new Error("--loopback runs ree's own loopback");
	// (known.json lists ree's differences: through rur every line must match)
	const known = options.rur ? {} : readKnown(options.server, options.loopback);
	const host = options.rur ? "rur" : "ree";
	const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "ree-parity-"));
	const rows = [];
	let failed = 0;
	for (const name of names) {
		const expected = readExpected(name, options.server, options.loopback);
		const ran = options.rur ? runInRur(name, options.rur, scratch) : options.loopback ? runInReeLoopback(name, options.ree, scratch) : runInRee(name, options.ree, scratch, options.server);
		if (!expected) {
			rows.push({ name, status: "no expected output", ran });
			continue;
		}
		const result = compare(name, expected.lines, ran.lines, known);
		const status = ran.errors.length ? "error" : result.unknown.length ? "diff" : result.known.length ? "known" : "pass";
		if (status === "diff" || status === "error") failed++;
		rows.push({ name, status, result, ran, header: expected.header });
	}
	fs.rmSync(scratch, { recursive: true, force: true });
	console.log(`${pad("probe", 22)} ${pad("status", 8)} ${pad("lines", 6)} ${pad("same", 6)} ${pad("known", 6)} ${pad("diff", 6)}`);
	for (const row of rows) {
		const r = row.result;
		console.log(`${pad(row.name, 22)} ${pad(row.status, 8)} ${pad(r ? r.total : "-", 6)} ${pad(r ? r.same : "-", 6)} ${pad(r ? r.known.length : "-", 6)} ${pad(r ? r.unknown.length : "-", 6)}`);
		for (const error of row.ran.errors) console.log(`    error: ${error}`);
		if (r && r.stale.length) console.log(`    known.json entries that now match (remove them): ${r.stale.join(" | ")}`);
	}
	const totals = rows.reduce((sum, row) => {
		if (row.result) {
			sum.lines += row.result.total;
			sum.same += row.result.same;
			sum.known += row.result.known.length;
			sum.diff += row.result.unknown.length;
		}
		return sum;
	}, { lines: 0, same: 0, known: 0, diff: 0 });
	console.log(`${pad("total", 22)} ${pad(failed ? "FAIL" : "ok", 8)} ${pad(totals.lines, 6)} ${pad(totals.same, 6)} ${pad(totals.known, 6)} ${pad(totals.diff, 6)}`);
	if (options.diff) {
		for (const row of rows) {
			if (!row.result || (!row.result.unknown.length && !row.result.known.length)) continue;
			console.log(`\n--- ${row.name} (expected: real engine, -; ${host}, +)`);
			const { diff } = row.result;
			const expected = readExpected(row.name, options.server, options.loopback).lines;
			for (const [kind, i, j] of diff) {
				if (kind === "-") console.log(`- ${expected[i]}`);
				else if (kind === "+") console.log(`+ ${row.ran.lines[j]}`);
			}
		}
	}
	if (options.writeKnown) {
		// every expected line ree does not print, keeping the reason already written for it (probes not run keep theirs)
		const next = Object.fromEntries(Object.entries(known).filter(([name]) => !rows.some(row => row.name === name && row.result)));
		for (const row of rows) {
			if (!row.result) continue;
			const lines = [...row.result.known, ...row.result.unknown];
			if (!lines.length) continue;
			next[row.name] = lines.map(line => {
				const old = (known[row.name] || []).find(entry => line.startsWith(entry.line));
				return { line, why: old ? old.why : "not fixed yet" };
			});
		}
		const sorted = Object.fromEntries(Object.keys(next).sort().map(name => [name, next[name]]));
		const knownFile = options.loopback ? KNOWN_LOOPBACK : options.server ? KNOWN_SERVER : KNOWN;
		fs.writeFileSync(knownFile, JSON.stringify(sorted, null, "\t") + "\n");
		console.log(`parity: wrote ${Object.values(next).reduce((sum, list) => sum + list.length, 0)} known differences to ${path.basename(knownFile)}`);
	}
	if (options.json) {
		fs.writeFileSync(options.json, JSON.stringify(rows.map(row => ({ name: row.name, status: row.status, errors: row.ran.errors, total: row.result?.total, same: row.result?.same, known: row.result?.known, unknown: row.result?.unknown, extra: row.result?.extra })), null, "\t"));
	}
	process.exitCode = failed ? 1 : 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();

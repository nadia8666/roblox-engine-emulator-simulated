// An installed ree stays standalone when --repo names input data. Its jobs, service and workers share one state dir.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { pixelPng, removeTestFolder, servedRun, startManualService, stopServices } from "../tools/release/smoke.mjs";

// Environment variables with these prefixes steer ree and its helpers, so the check starts without them
const STEERING_VARIABLES = /^(REE_|RUR_|ROR_)/i;

const HARNESS = `return function()
local assets = game:GetService("AssetService")
local image = assets:CreateEditableImageAsync(Content.fromUri("pixel.png"))
assert(image.Size == Vector2.one, "local images come from the client's project")
assert(not game.ReplicatedStorage:FindFirstChild("SnapshotMarker"), "no implicit repo snapshots")
image:DrawRectangle(Vector2.zero, Vector2.one, Color3.new(1, 0, 0), 0, Enum.ImageCombineType.Overwrite)
assets:CreateSurfaceAppearanceAsync({ColorMap = Content.fromObject(image)})
print("standalone project ok")
end`;

// Copies ree outside its checkout, redirects user folders, and removes all files after a passing check.
function check(name, body) {
	test(name, async () => {
		// the real path of the temp folder: ree reports paths with links resolved, and the checks compare against them
		const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "ree-service-cache-")));
		const executable = path.join(root, process.platform === "win32" ? "ree.exe" : "ree");
		fs.copyFileSync(process.env.REE_TEST_BINARY, executable);
		fs.chmodSync(executable, 0o755);
		const inputs = path.join(root, "inputs");
		const project = path.join(root, "project");
		for (const dir of [inputs, project, path.join(root, "temp")]) fs.mkdirSync(dir);
		fs.writeFileSync(path.join(inputs, "Cargo.lock"), "");
		fs.writeFileSync(path.join(inputs, "Cargo.toml"), '[package]\nname = "another-tool"\n');
		fs.writeFileSync(path.join(project, "Harness.luau"), HARNESS);
		fs.writeFileSync(path.join(project, "pixel.png"), pixelPng());
		// An old snapshot under the input repo must never become a standalone state fallback.
		const snapshots = path.join(inputs, ".cache", "snapshots", "TESTING");
		fs.mkdirSync(snapshots, { recursive: true });
		fs.writeFileSync(path.join(snapshots, "ReplicatedStorage.rbxmx"), '<roblox version="4"><Item class="Folder" referent="RBX0"><Properties><string name="Name">SnapshotMarker</string></Properties></Item></roblox>');
		const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !STEERING_VARIABLES.test(key)));
		Object.assign(env, {
			LOCALAPPDATA: path.join(root, "local"),
			HOME: path.join(root, "home"),
			USERPROFILE: path.join(root, "home"),
			XDG_CACHE_HOME: path.join(root, "xdg"),
			TMP: path.join(root, "temp"), TEMP: path.join(root, "temp"), TMPDIR: path.join(root, "temp"),
			REE_PARENT_WATCH: "0", REE_SERVE_WORKERS: "1",
		});
		const run = (args, cwd = project) => {
			const result = spawnSync(executable, args, { cwd, env, encoding: "utf8", timeout: 120_000, windowsHide: true });
			assert.ifError(result.error);
			return result;
		};
		const ree = (args, cwd) => {
			const result = run(args, cwd);
			assert.equal(result.status, 0, result.stderr || result.stdout);
			return result.stdout;
		};
		let passed = false;
		try {
			await body({ root, inputs, project, env, ree, run, executable });
			passed = true;
		} finally {
			// Bootstrap failures have no usable state folder and cannot have started a service. A failure to stop one keeps
			// the folder, and fails the check only if its own body had passed.
			try {
				if (env.LOCALAPPDATA || env.HOME || env.RUR_STATE_DIR || env.RUR_CACHE_DIR) await stopServices(ree);
			} catch (error) {
				console.error(`service cleanup failed; files kept at ${root}: ${error.message}`);
				if (passed) throw error;
			}
			if (passed) await removeTestFolder(root);
			else console.error(`service cache check failed; files kept at ${root}`);
		}
	});
}

// Every file and directory under `root` with its content hash, apart from the folders a command may write in. A command
// that leaves this list unchanged wrote nothing else.
function contentsOf(root, writable = []) {
	const entries = [];
	function visit(folder) {
		for (const entry of fs.readdirSync(folder, { withFileTypes: true })) {
			const file = path.join(folder, entry.name);
			if (writable.includes(file)) continue;
			const hash = entry.isDirectory() ? null : createHash("sha256").update(fs.readFileSync(file)).digest("hex");
			entries.push([path.relative(root, file), hash]);
			if (entry.isDirectory()) visit(file);
		}
	}
	visit(root);
	return entries;
}

// The records of the running services, one per line of `serve --status` (no line when none runs)
function runningServices(ree, cwd) {
	return ree(["serve", "--status"], cwd).trim().split("\n").filter(Boolean).map(JSON.parse);
}

const VARIANTS ='return { sizes = {1}, variants = {{name = "one", run = function(n) return n end}} }';
const MISSING_CACHE_ERROR = "cannot find a per-user cache folder; set RUR_STATE_DIR to a writable folder";

// Where the per-user cache of ree is on this platform, given the redirected user folders in `env`
function perUserCache(env) {
	if (process.platform === "win32") return path.join(env.LOCALAPPDATA, "ree");
	if (process.platform === "darwin") return path.join(env.HOME, "Library/Caches/ree");
	return path.join(env.HOME, ".cache/ree");
}

// Stops the services of a check and then the service the check started by hand, if any
async function stopAllServices(ree, started) {
	try {
		await stopServices(ree);
	} finally {
		if (started) {
			if (started.service.exitCode === null) started.service.kill();
			await started.closed;
		}
	}
}

// A relative XDG_CACHE_HOME is ignored, so the cache is HOME/.cache/ree on Linux. A service normalizes its environment
// and changes its folder, so every job it runs must still use the cache the client's environment chose.
for (const manual of [false, true]) check(`relative XDG_CACHE_HOME keeps the per-user fallback, manual=${manual}`, async ({ root, project, env, ree, executable }) => {
	env.XDG_CACHE_HOME = "relative";
	const state = perUserCache(env);
	fs.mkdirSync(path.dirname(state), { recursive: true });
	const before = contentsOf(root, [state]);
	const job = ["--harness", "Harness.luau", "--seconds", "0.05", "--offline"];
	const direct = JSON.parse(ree(["run", ...job]));
	assert.equal(direct.errors, 0);
	// every job leaves an image in the state it used, so the count shows which state that was
	const images = () => fs.readdirSync(path.join(state, "editable-images")).length;
	assert.equal(images(), 1);
	// A lone --service call runs in the client; the next call in its burst starts the service.
	if (!manual) {
		const firstServiceFallback = ree(["run", "--service", ...job]);
		assert.equal(JSON.parse(firstServiceFallback).errors, 0);
	}
	const imagesBeforeService = images();
	const started = manual ? startManualService(executable, project, env) : null;
	try {
		if (started) await started.listening;
		let servicePid;
		for (let attempt = 0; attempt < 2; attempt++) {
			const served = JSON.parse(await servedRun(ree, ["run", "--service", ...job], servicePid ?? started?.service.pid));
			assert.equal(served.errors, 0);
			assert.deepEqual(served.output, direct.output);
			assert.equal(images(), imagesBeforeService + attempt + 1, "jobs keep the state chosen at service startup");
			const services = runningServices(ree);
			assert.equal(services.length, 1);
			servicePid ??= started?.service.pid ?? services[0].pid;
			assert.equal(services[0].pid, servicePid);
		}
		assert.equal(JSON.parse(await servedRun(ree, ["serve", "--warm", ...job], servicePid)).warm, true);
		assert.equal(runningServices(ree)[0].pid, servicePid, "warm-up finds the same service");
		const warmServed = JSON.parse(await servedRun(ree, ["run", "--service", ...job], servicePid));
		assert.equal(warmServed.errors, 0);
		assert.deepEqual(warmServed.output, direct.output);
		assert.equal(images(), imagesBeforeService + 3, "the warm run keeps the same state");
		assert(fs.readdirSync(path.join(state, "record")).length > 0);
		assert.deepEqual(contentsOf(root, [state]), before, "writes stay in the chosen per-user cache");
	} finally {
		await stopAllServices(ree, started);
	}
});

for (const kind of ["work,cpu", "boundary", "environment"]) {
	check(`unnamed ${kind} profiles stay in standalone state`, ({ root, inputs, env, run }) => {
		env.RUR_STATE_DIR = path.join(root, "state");
		const before = contentsOf(root, [env.RUR_STATE_DIR]);
		const args = ["run", "--repo", inputs, "--harness", "Harness.luau", "--seconds", "0.05", "--offline"];
		if (kind === "environment") env.REE_PROFILE = "work";
		else args.push("--profile", kind);
		const result = run(args);
		assert.equal(result.status, 0, result.stderr);
		// the user never named this folder, so the run says where the profile went
		const profile = kind === "boundary" ? "ree-boundary.json" : "ree-profile/profile.json";
		const output = path.join(env.RUR_STATE_DIR, profile);
		assert(fs.existsSync(output), `missing profile ${output}\n${result.stderr}`);
		assert(result.stderr.includes(path.dirname(output)), result.stderr);
		assert.deepEqual(contentsOf(root, [env.RUR_STATE_DIR]), before);
	});
}

for (const fileInput of [false, true]) check(`profile diff and report keep unnamed outputs in state, fileInput=${fileInput}`, ({ root, project, env, run }) => {
	env.RUR_STATE_DIR = path.join(root, "state");
	for (const name of ["before", "after"]) {
		fs.mkdirSync(path.join(project, name));
		fs.writeFileSync(path.join(project, name, "profile.json"), '{"summary":"saved profile"}');
	}
	const beforeInput = fileInput ? "before/profile.json" : "before";
	const afterInput = fileInput ? "after/profile.json" : "after";
	const namedOut = path.join(project, "comparison");
	const stateOut = path.join(env.RUR_STATE_DIR, "ree-profile");
	const unchanged = contentsOf(root, [env.RUR_STATE_DIR, namedOut]);

	// a diff goes to the folder `--out` names, else to state, and the command says where
	for (const out of [null, namedOut]) {
		const diff = run(["profile", "diff", beforeInput, afterInput, ...(out ? ["--out", out] : [])]);
		assert.equal(diff.status, 0, diff.stderr);
		const directory = out ?? stateOut;
		for (const file of ["diff.txt", "diff.json", "diff.html"]) assert(fs.existsSync(path.join(directory, file)), diff.stdout);
		assert(diff.stdout.includes(directory), diff.stdout);
	}

	// a report goes to state too, not beside the profile it was made from
	const report = run(["profile", "report", afterInput]);
	assert.equal(report.status, 0, report.stderr);
	const page = path.join(stateOut, "report.html");
	assert(fs.existsSync(page), report.stdout);
	assert(report.stdout.includes(page), report.stdout);

	// nothing else was written, in the inputs' folders above all
	assert.deepEqual(contentsOf(root, [env.RUR_STATE_DIR, namedOut]), unchanged);
});

check("checkout profile diff and report keep their host defaults", ({ root, project, run }) => {
	// a Cargo.lock and a Cargo.toml naming this package above the binary make it a binary in its own checkout
	fs.writeFileSync(path.join(root, "Cargo.lock"), "");
	fs.writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "roblox-engine-emulator"\n');
	const afterFolder = path.join(project, "after");
	fs.writeFileSync(path.join(project, "before.json"), '{"summary":"before"}');
	fs.mkdirSync(afterFolder);
	fs.writeFileSync(path.join(afterFolder, "profile.json"), '{"summary":"after"}');

	// the diff lands in the after folder and the report beside its profile, as before
	assert.equal(run(["profile", "diff", "before.json", "after"]).status, 0);
	for (const file of ["diff.txt", "diff.json", "diff.html"]) assert(fs.existsSync(path.join(afterFolder, file)));
	const report = run(["profile", "report", "after"]);
	assert.equal(report.status, 0, report.stderr);
	assert.equal(report.stdout, "after");
	assert(fs.existsSync(path.join(afterFolder, "report.html")));
});

check("relative dump and clock paths keep their client meaning through a warm service", async ({ root, project, env, ree }) => {
	// The paths point one folder above the project. The service runs in the state dir, two folders below the root,
	// where the same relative names would reach cache/dump.txt and nothing for the clock.
	env.RUR_STATE_DIR = path.join(root, "cache/state");
	env.RUR_ENGINE_DUMP = "../dump.txt";
	env.RUR_CLOCK = "../clock.json";
	const dumpFile = path.join(root, "dump.txt");
	fs.writeFileSync(path.join(root, "clock.json"), JSON.stringify({ version: 2, mount: { start: 500, end: 500 }, unix: 1790000000000, events: [] }));
	fs.writeFileSync(path.join(project, "Harness.luau"), 'return function() assert(os.clock() >= 500 and os.clock() < 501, "the recorded clock was loaded") print("client clock ok") end');
	const args = ["--harness", "Harness.luau", "--seconds", "0.01", "--offline"];

	// a direct run is the reference for what the service must do
	const direct = JSON.parse(ree(["run", ...args]));
	assert.equal(direct.errors, 0, JSON.stringify(direct));
	const dump = fs.readFileSync(dumpFile, "utf8");
	assert(dump.includes("DataModel"));

	assert.equal(JSON.parse(await servedRun(ree, ["serve", "--warm", ...args])).warm, true);
	const service = JSON.parse(ree(["serve", "--status"]));
	let jobsDone = service.health.jobs.done;
	for (const absolute of [false, true]) {
		if (absolute) {
			// the same files spelled absolutely, with a `..` so the spelling differs from the relative form too
			env.RUR_ENGINE_DUMP = `${project}${path.sep}..${path.sep}dump.txt`;
			env.RUR_CLOCK = `${project}${path.sep}..${path.sep}clock.json`;
		}
		fs.unlinkSync(dumpFile);
		const served = JSON.parse(await servedRun(ree, ["run", "--service", ...args], service.pid));
		assert.equal(served.errors, 0, JSON.stringify(served));
		assert.deepEqual(served.output, direct.output);
		assert.equal(fs.readFileSync(dumpFile, "utf8"), dump);
		const services = runningServices(ree);
		assert.equal(services.length, 1, "relative and absolute paths find the same service");
		assert.equal(services[0].pid, service.pid);
		assert.equal(services[0].health.jobs.done, ++jobsDone, "the warm service answered the job");
	}
	assert(!fs.existsSync(path.join(root, "cache/dump.txt")));
});

check("relative environment outputs distinguish services by client folder", async ({ root, project, env, ree }) => {
	env.RUR_STATE_DIR = path.join(root, "state");
	env.RUR_ENGINE_DUMP = "dump.txt";
	env.RUR_BOUNDARY_PROFILE = "boundary.json";
	// a second client folder with the same project files
	const second = path.join(root, "second");
	fs.mkdirSync(second);
	fs.copyFileSync(path.join(project, "Harness.luau"), path.join(second, "Harness.luau"));
	fs.copyFileSync(path.join(project, "pixel.png"), path.join(second, "pixel.png"));
	const args = ["--harness", "Harness.luau", "--seconds", "0.01", "--offline"];

	// the same relative names, run from each folder, each write into that folder
	for (const folder of [project, second]) {
		// each folder's relative names make its own service: the warm-up must start a new one, and the run must be
		// completed by that one and not by the other folder's
		const inFolder = (argv) => ree(argv, folder);
		const pidsBefore = new Set(runningServices(ree).map((running) => running.pid));
		const warmReply = await servedRun(inFolder, ["serve", "--warm", ...args], undefined, { newService: true });
		assert.equal(JSON.parse(warmReply).warm, true);
		const service = runningServices(ree).find((running) => !pidsBefore.has(running.pid));
		const runReply = await servedRun(inFolder, ["run", "--service", ...args], service.pid);
		assert.equal(JSON.parse(runReply).errors, 0);
		assert(fs.existsSync(path.join(folder, "dump.txt")));
	}

	// each folder keyed its own service (the warm-up and the run share one, so each answered two jobs)
	const services = runningServices(ree);
	assert.equal(services.length, 2, "the same relative names refer to different files in each client folder");
	assert(services.every((info) => info.health.jobs.done === 2), JSON.stringify(services));
	assert(!fs.existsSync(path.join(env.RUR_STATE_DIR, "dump.txt")));
});

check("profile try defaults to standalone state and honors an explicit output", ({ root, inputs, project, env, run }) => {
	env.RUR_STATE_DIR = path.join(root, "state");
	fs.writeFileSync(path.join(project, "Variants.luau"), VARIANTS);
	const explicit = path.join(project, "comparison");
	const before = contentsOf(root, [env.RUR_STATE_DIR, explicit]);
	for (const out of [null, explicit]) {
		const args = ["profile", "try", "Variants.luau", "--repo", inputs, "--frames", "1", "--no-time", "--no-hw", "--no-check", "--offline"];
		if (out) args.push("--out", out);
		const result = run(args);
		assert.equal(result.status, 0, result.stderr || result.stdout);
		const comparison = out ?? path.join(env.RUR_STATE_DIR, "what-if/Variants");
		assert(fs.existsSync(path.join(comparison, "compare.json")), result.stdout);
		assert(result.stdout.includes(comparison), result.stdout);
	}
	assert.deepEqual(contentsOf(root, [env.RUR_STATE_DIR, explicit]), before);
});

check("explicit profiles and recording output aliases keep their paths", ({ root, inputs, project, env, run }) => {
	env.RUR_STATE_DIR = path.join(root, "state");
	const named = path.join(project, "named");
	fs.mkdirSync(named);
	const before = contentsOf(root, [env.RUR_STATE_DIR, named]);
	const args = ["--repo", inputs, "--harness", "Harness.luau", "--seconds", "0.05", "--offline"];
	// --profile-out is a folder for the work profile and a file for the boundary report
	for (const kind of ["work", "boundary"]) {
		const profileOut = path.join(named, kind === "boundary" ? "crossings.json" : "suite");
		const result = run(["run", ...args, "--profile", kind, "--profile-out", profileOut]);
		assert.equal(result.status, 0, result.stderr);
		assert(fs.existsSync(kind === "boundary" ? profileOut : path.join(profileOut, "profile.json")));
	}
	// a recording's profile goes in its own output folder, whichever flag names that folder
	for (const flag of ["--out", "--dir", "--dump-scene"]) {
		const recording = path.join(named, flag.slice(2));
		const result = run(["record", ...args, flag, recording, "--profile", "work"]);
		assert.equal(result.status, 0, result.stderr);
		assert(fs.existsSync(path.join(recording, "scene.rbxm")));
		assert(fs.existsSync(path.join(recording, "profile/profile.json")));
	}
	assert.deepEqual(contentsOf(root, [env.RUR_STATE_DIR, named]), before);
});

check("a warm standalone service reads relative frame times from the client", async ({ inputs, project, ree }) => {
	fs.writeFileSync(path.join(project, "frames.json"), "[0,0.01,0.02]");
	const args = ["--repo", inputs, "--harness", "Harness.luau", "--frame-times=frames.json", "--seconds", "0.02", "--offline"];
	const direct = JSON.parse(ree(["run", ...args]));
	assert.equal(JSON.parse(await servedRun(ree, ["serve", "--warm", ...args])).warm, true);
	const service = JSON.parse(ree(["serve", "--status"]));
	const served = JSON.parse(await servedRun(ree, ["run", "--service", ...args], service.pid));
	assert.equal(served.errors, 0);
	assert.deepEqual(served.output, direct.output);
	assert.equal(served.frames, direct.frames);
	assert.equal(JSON.parse(ree(["serve", "--status"])).pid, service.pid, "the same warm service handled the job");
});

check("an input repo never substitutes for a missing per-user cache", async ({ root, inputs, project, env, ree, run }) => {
	for (const name of Object.keys(env)) if (/^(LOCALAPPDATA|XDG_CACHE_HOME|HOME|USERPROFILE)$/i.test(name)) delete env[name];
	assert.match(ree(["--version"]), /^ree \d+\.\d+\.\d+/);
	assert.match(ree(["--help"]), /^usage: ree/);
	assert.match(ree(["serve", "--help"]), /^usage: ree serve/);
	// Reports and diffs also need state for their unnamed outputs.
	fs.writeFileSync(path.join(project, "profile.json"), '{"summary":"report without state"}');
	fs.writeFileSync(path.join(project, "Variants.luau"), VARIANTS);
	const args = ["--repo", inputs, "--harness", "Harness.luau", "--offline", "--seconds", "0.05"];
	// every command that needs state, with and without an explicit output: each fails before it writes anything
	const commands = [
		["profile", "report", project],
		["profile", "diff", project, project],
		["profile", "diff", project, project, "--out", "comparison"],
		["run", ...args],
		["record", ...args, "--out", "recording"],
		["run", ...args, "--profile", "work"],
		["run", ...args, "--profile", "work,cpu"],
		["run", ...args, "--profile", "boundary"],
		["run", ...args, "--profile", "work", "--profile-out", "profile-out"],
		["record", ...args, "--out", "recording", "--profile", "work,cpu"],
		["profile", "try", "Variants.luau"],
		["profile", "try", "Variants.luau", "--out", "comparison"],
		["run", "--service", ...args],
		["run", "--service", ...args, "--profile", "work"],
		["record", "--service", ...args, "--out", "recording"],
		["serve"],
		["serve", "--status"],
		["serve", "--stop"],
		["serve", "--warm", ...args],
	];
	const before = contentsOf(root);
	for (const command of commands) {
		const result = run(command);
		assert.notEqual(result.status, 0);
		assert.match(result.stderr, new RegExp(`^ree(?: run| record| profile (?:try|diff|report)| serve| serve --warm)?: ${MISSING_CACHE_ERROR}\\r?\\n$`));
		assert.deepEqual(contentsOf(root), before, `writes before the missing-cache error: ${command.join(" ")}`);
	}
	// profiling that an environment variable switches on must not write before the error either
	for (const variable of ["REE_PROFILE", "RUR_BOUNDARY_PROFILE", "RUR_LUAU_PROFILE", "RUR_LUAU_ALLOC_PROFILE"]) {
		env[variable] = variable === "REE_PROFILE" ? "work" : "profile-from-env";
		const result = run(["run", "--service", ...args]);
		assert.match(result.stderr, new RegExp(`^ree(?: run)?: ${MISSING_CACHE_ERROR}\\r?\\n$`));
		assert.notEqual(result.status, 0);
		assert.deepEqual(contentsOf(root), before, `writes before the missing-cache error with ${variable}`);
		delete env[variable];
	}
	assert(!fs.existsSync(path.join(inputs, ".cache", "editable-images")));
	// An explicit cache also works without any per-user folder, including service startup and warm-up.
	env.RUR_STATE_DIR = path.join(project, "state");
	assert.equal(JSON.parse(ree(["run", ...args])).errors, 0);
	assert.equal(JSON.parse(await servedRun(ree, ["serve", "--warm", ...args])).warm, true);
	assert.equal(JSON.parse(await servedRun(ree, ["run", "--service", ...args])).errors, 0);
});

for (const override of [null, "RUR_CACHE_DIR", "RUR_STATE_DIR"]) check(`--repo keeps standalone state, override=${override}`, async ({ root, inputs, project, env, ree }) => {
	const userCache = process.platform === "win32" ? path.join(env.LOCALAPPDATA, "ree")
		: process.platform === "darwin" ? path.join(env.HOME, "Library/Caches/ree") : path.join(env.XDG_CACHE_HOME, "ree");
	if (override) env[override] = path.join(root, "override");
	const state = override ? env[override] : userCache;
	const job = ["--repo", "", "--repo", inputs, "--harness", "Harness.luau", "--seconds", "0.05", "--offline"];
	const images = () => fs.readdirSync(path.join(state, "editable-images")).length;
	const direct = JSON.parse(ree(["run", ...job]));
	assert.equal(direct.errors, 0, JSON.stringify(direct));
	assert.equal(images(), 1);
	// Each service call gives the output of the direct run and adds one image. The first call may run in the client and
	// start the service; the next must be served by it.
	const assertServiceRun = (output, imageCount) => {
		const served = JSON.parse(output);
		assert.equal(served.errors, 0, JSON.stringify(served));
		assert.deepEqual(served.output, direct.output);
		assert.equal(images(), imageCount);
	};
	const firstServiceFallback = ree(["run", "--service", ...job]);
	assertServiceRun(firstServiceFallback, 2);
	assertServiceRun(await servedRun(ree, ["run", "--service", ...job]), 3);
	const before = ree(["serve", "--status"]).trim().split("\n").map(JSON.parse);
	assert.equal(before.length, 1, "the burst started one service");
	// Empty and repeated roots must retain the current-project default during warm-up too.
	for (const roots of [[], ["--roots", ""], ["--roots", ";;"], ["--roots", "unused", "--roots", ""]]) {
		assert.equal(JSON.parse(await servedRun(ree, ["serve", "--warm", ...job, ...roots], before[0].pid)).warm, true);
	}
	assert.deepEqual(ree(["serve", "--status"]).trim().split("\n").map(JSON.parse).map((info) => info.pid), before.map((info) => info.pid));
	assert(fs.readdirSync(path.join(state, "record")).length > 0, "run history uses standalone state too");
	assert(!fs.existsSync(path.join(inputs, ".cache", "editable-images")));
	assert(!fs.existsSync(path.join(inputs, ".cache", "record")));
	assert(!fs.existsSync(path.join(project, ".cache")));
});

// Process tests for the installed asset worker, with a local JSON worker and no downloads.
//
// Each check copies the ree binary into a fresh folder and runs it there, so it is a standalone install. The worker is
// a Node launcher that a --require script turns into a mock worker, and it logs every request it gets to a file. The
// checks read that log to see which executable ran and which paths it was sent.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { realPath } from "../tools/release/common.mjs";
import { removeTestFolder, servedRun, startManualService, stopServices, waitForProcesses } from "../tools/release/smoke.mjs";

// Environment variables with these prefixes steer ree and its helpers, so a check starts without them
const STEERING_VARIABLES = /^(REE_|RUR_|ROR_)/i;

const WINDOWS = process.platform === "win32";
const WORKER_FILE = WINDOWS ? "worker.exe" : "worker";
const HOST_WORKER_VARIABLE = "REE_ASSET_WORKER";

// The per-user and temporary folder variables ree resolves against the client's folder before a worker sees them
const USER_FOLDER_VARIABLES = ["HOME", "USERPROFILE", "LOCALAPPDATA", "XDG_CACHE_HOME", "APPDATA", "TMP", "TEMP", "TMPDIR"];

// The model every asset or bundle reply holds
const MODEL_XML = '<roblox version="4"><Item class="KeyframeSequence" referent="RBX0"><Properties><string name="Name">Mock</string></Properties></Item></roblox>';

// Answers the worker protocol: one JSON request a line on stdin, one JSON reply a line on stdout. A catalog request
// lists one bundle; an asset or bundle request writes a small KeyframeSequence model to the requested `out` file. Each
// request is logged with the executable and folder that ran it and the PATH, user folders and worker variable it saw. Three
// variables change the reply:
//   REE_REPLY_RELATIVE=1  the reply names its file relative to the worker's folder, as a worker that keeps its files
//                         beside itself would
//   REE_REPLY_CACHED      the reply names this file, a cached copy of the worker's own, instead of `out`
//   REE_SKIP_OUTPUT=1     the worker does not write `out`, as when it answers from its own cache
const MOCK_WORKER = `
const fs = require("node:fs");
const path = require("node:path");
// With REE_WORKER_HELPER=1 this process is the helper tool: it only says it ran.
if (process.env.REE_WORKER_HELPER === "1") {
	console.log("helper ok");
	process.exit(0);
}
// With REE_WORKER_USES_HELPER=1 the worker starts a tool by its bare name "helper" first, as a real worker would start
// its own helpers: the lookup uses the PATH ree gave the worker.
if (process.env.REE_WORKER_USES_HELPER === "1") {
	const helperEnv = { ...process.env, REE_WORKER_HELPER: "1" };
	const result = require("node:child_process").spawnSync("helper", [], { encoding: "utf8", env: helperEnv });
	if (result.error || result.status !== 0 || !result.stdout.includes("helper ok")) throw new Error("helper failed");
}
require("node:readline").createInterface({ input: process.stdin }).on("line", (line) => {
	const request = JSON.parse(line);
	// The test roots are canonical, but the OS may report this executable through a link (macOS /var is /private/var),
	// so log its real path to compare with the paths the test built. On Unix the worker is a launcher script that runs
	// node with its own path as argv[1] (installWorker), because process.execPath is then the shared node.
	const worker = process.platform === "win32" ? process.execPath : process.argv[1];
	const logged = {
		...request,
		pid: process.pid,
		worker: fs.realpathSync.native(worker),
		cwd: process.cwd(),
		workerEnv: process.env.REE_ASSET_WORKER,
		pathEnv: process.env.PATH,
		nodePath: process.env.NODE_PATH,
		moduleValue: process.env.WORKER_MODULE ? require("worker-tools") : undefined,
		customValue: process.env.WORKER_VALUE,
		customRelative: process.env.WORKER_RELATIVE,
		userFolders: Object.fromEntries(${JSON.stringify(USER_FOLDER_VARIABLES)}.map((name) => [name, process.env[name]])),
		drives: { HOMEDRIVE: process.env.HOMEDRIVE, SystemDrive: process.env.SystemDrive },
	};
	fs.appendFileSync(process.env.REE_WORKER_LOG, JSON.stringify(logged) + "\\n");
	if (request.command === "catalog") { console.log(JSON.stringify({ bundles: ["Models/Props"] })); return; }
	if (process.env.REE_SKIP_OUTPUT !== "1") {
		fs.mkdirSync(path.dirname(request.out), { recursive: true });
		fs.writeFileSync(request.out, ${JSON.stringify(MODEL_XML)});
	}
	const reported = process.env.REE_REPLY_CACHED || request.out;
	const file = process.env.REE_REPLY_CANONICAL === "1" ? fs.realpathSync.native(reported)
		: process.env.REE_REPLY_RELATIVE === "1" ? path.relative(process.cwd(), reported) : reported;
	console.log(JSON.stringify({ file }));
});
`;

// Puts a mock worker (or helper, or decoy) at `file`, with the file mode `mode`.
//
// Windows copies node itself. On Unix a copy would not start: Homebrew's node on macOS finds its libnode library
// relative to where it is installed (@loader_path/../lib), so a lone copy makes dyld exit before the mock runs. The
// file is a small launcher instead, which runs the installed node by absolute path. It can't be `#!/usr/bin/env node`,
// because several checks set PATH to only the worker and helper folders, where `node` is not found.
//
// The launcher passes its own path to node as argv[1], and the mock logs that as the worker: a log of the shared node
// would not show which of the files on PATH the OS picked. The empty `-e` program stops node from treating "$0" as a
// script to run, so stdin is left to the mock that --require loads.
function installWorker(file, mode = 0o755) {
	if (WINDOWS) {
		fs.copyFileSync(process.execPath, file);
	} else {
		const node = `'${process.execPath.replaceAll("'", "'\\''")}'`; // single-quoted for sh
		fs.writeFileSync(file, `#!/bin/sh\nexec ${node} -e '' "$0" "$@"\n`);
	}
	fs.chmodSync(file, mode);
}

// Runs `body` in a fresh folder with an installed ree. It gets:
//   env       the environment ree runs in; a check may add to it
//   ree       (cwd, args) runs ree and returns its output
//   project   (name) makes a project folder holding a worker at helpers/worker
//   requests  () the requests the worker has logged so far
function check(name, body) {
	test(name, async () => {
		// the real path of the temp folder: ree reports paths with links resolved, and the checks compare against them
		const root = realPath(fs.mkdtempSync(path.join(os.tmpdir(), "ree-worker-test-")));
		const executable = path.join(root, WINDOWS ? "ree.exe" : "ree");
		fs.copyFileSync(process.env.REE_TEST_BINARY, executable);
		fs.chmodSync(executable, 0o755);

		const mock = path.join(root, "mock.cjs");
		fs.writeFileSync(mock, MOCK_WORKER);
		const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !STEERING_VARIABLES.test(key)));
		Object.assign(env, {
			REE_PARENT_WATCH: "0",
			REE_SERVE_WORKERS: "1",
			RUR_STATE_DIR: path.join(root, "state"),
			REE_WORKER_LOG: path.join(root, "requests.jsonl"),
			NODE_OPTIONS: `--require=${JSON.stringify(mock)}`,
		});

		const ree = (cwd, args) => {
			const result = spawnSync(executable, args, { cwd, env, encoding: "utf8", timeout: 120_000, windowsHide: true });
			assert.ifError(result.error);
			assert.equal(result.status, 0, result.stderr || result.stdout);
			return result.stdout;
		};
		const project = (folder) => {
			const directory = path.join(root, folder);
			fs.mkdirSync(path.join(directory, "helpers"), { recursive: true });
			const worker = path.join(directory, "helpers", WORKER_FILE);
			installWorker(worker);
			return directory;
		};
		const requests = () => fs.readFileSync(env.REE_WORKER_LOG, "utf8").trim().split("\n").map(JSON.parse);

		// A passing check removes its folder; a failing one keeps it and says where it is
		let passed = false;
		try {
			await body({ root, env, ree, project, requests, executable });
			passed = true;
		} finally {
			// Stop the services, then wait for the workers they started, so nothing holds the folder when it is removed.
			// A failure here keeps the folder, and fails the check only if its own body had passed.
			try {
				await stopServices((args) => ree(root, args));
				// the log exists only when a worker ran
				if (fs.existsSync(env.REE_WORKER_LOG)) await waitForProcesses(requests().map((row) => row.pid));
			} catch (error) {
				console.error(`worker cleanup failed; files kept at ${root}: ${error.message}`);
				if (passed) throw error;
			}
			if (passed) await removeTestFolder(root);
			else console.error(`asset worker check failed; files kept at ${root}`);
		}
	});
}

// Removes PATH from `env`. Windows spells it "Path" and a plain copy of the environment keeps that spelling.
function removePath(env) {
	for (const key of Object.keys(env)) {
		if (key.toUpperCase() === "PATH") delete env[key];
	}
}

// Writes a harness in `project` that fetches animation `id` through the worker and returns the `ree` arguments that run it,
// directly or through the service
function animationJob(project, id, viaService = false, args = []) {
	fs.writeFileSync(path.join(project, "Harness.luau"), `return function()
local sequence = game:GetService("KeyframeSequenceProvider"):GetKeyframeSequenceAsync("rbxassetid://${id}")
assert(sequence.Name == "Mock")
print("worker asset ok")
end`);
	return ["run", "--harness", "Harness.luau", "--seconds", "0.05", ...args, ...(viaService ? ["--service"] : [])];
}

// Checks that the worker answered an animation job
function assertAnimation(output) {
	assert.match(output, /worker asset ok/);
	assert.equal(JSON.parse(output).errors, 0, output);
}

// `ree` as servedRun takes it: only the arguments, run from `directory`
function inDirectory(ree, directory) {
	return (args) => ree(directory, args);
}

// Runs the animation job. A direct job only has to be answered by the worker; a job through the service must also be
// completed by it, which servedRun proves: `expectedPid` names that service, or the one running service is expected.
async function fetchAnimation(ree, project, id, viaService = false, args = [], expectedPid) {
	const job = animationJob(project, id, viaService, args);
	const output = viaService ? await servedRun(inDirectory(ree, project), job, expectedPid) : ree(project, job);
	assertAnimation(output);
}

// With no service running, the first `--service` call runs the job in the client (and starts the service), so it can't
// be held to servedRun the way the calls after it are.
function fetchAnimationFallback(ree, project, id, args = []) {
	assertAnimation(ree(project, animationJob(project, id, true, args)));
}

// Warms a service, waits until it has finished that job and returns its PID. With no `expectedPid` the warm-up must
// start a new service, as a changed environment or client folder does; with one, that service must do the job.
async function warmService(ree, directory, args = [], expectedPid) {
	const servicesBefore = serviceJobCounts(ree, directory);
	const warmUp = ["serve", "--warm", "--roots", "root", ...args];
	const startsNewService = expectedPid === undefined;
	const reply = await servedRun(inDirectory(ree, directory), warmUp, expectedPid, { newService: startsNewService });
	assert.equal(JSON.parse(reply).warm, true);
	if (!startsNewService) return expectedPid;
	return [...serviceJobCounts(ree, directory).keys()].find((pid) => !servicesBefore.has(pid));
}

// The completed-job count of each running service, by process id
function serviceJobCounts(ree, directory) {
	const rows = ree(directory, ["serve", "--status"]).trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
	return new Map(rows.map((row) => [row.pid, row.health.jobs.done]));
}

// What the worker reports about its environment, which a direct and a served run must agree on
const WORKER_VIEW_FIELDS = ["nodePath", "moduleValue", "customValue", "customRelative", "cwd"];

// The worker is a Node script that reports its environment. A service is keyed by everything the worker receives, so
// each environment below differs from the one before it (NODE_PATH, then WORKER_VALUE set, then removed) and must
// get a service of its own. `servicesAfter` is how many automatic services are running once it has run.
for (const manual of [false, true]) check(`workers receive the full client environment after path resolution, manual=${manual}`, async ({ root, env, ree, project, requests, executable }) => {
	const directory = project("project");
	const reeInProject = (args) => ree(directory, args);
	fs.mkdirSync(path.join(directory, "root"));
	env.REE_ASSET_WORKER = path.join(directory, "helpers", WORKER_FILE);
	// two folders that each hold a module of the same name, so the worker's `require` shows which NODE_PATH entry it got
	for (const name of ["first", "second"]) {
		const modules = path.join(directory, name);
		fs.mkdirSync(modules);
		fs.writeFileSync(path.join(modules, "worker-tools.js"), `module.exports = ${JSON.stringify(name)};`);
	}
	env.WORKER_MODULE = "1";
	// a variable ree does not know holds a relative value: it is passed unchanged and read from the state dir
	env.WORKER_RELATIVE = "unlisted-relative-value";
	const environments = [
		{ nodePath: "first", value: "one", servicesAfter: 1 },
		{ nodePath: path.join(directory, "second"), value: "two", servicesAfter: 2 },
		{ nodePath: path.join(directory, "second"), value: undefined, servicesAfter: 3 },
	];
	let id = 8000;
	for (const { nodePath, value, servicesAfter } of environments) {
		env.NODE_PATH = nodePath;
		if (value === undefined) delete env.WORKER_VALUE;
		else env.WORKER_VALUE = value;

		// the direct run: NODE_PATH is resolved against the client's folder, the custom values arrive as they are
		await fetchAnimation(ree, directory, ++id);
		const direct = requests().at(-1);
		assert.equal(direct.nodePath, path.resolve(directory, nodePath));
		assert.equal(direct.moduleValue, path.basename(nodePath));
		assert.equal(direct.customValue, value);
		assert.equal(direct.customRelative, env.WORKER_RELATIVE);
		assert.equal(direct.cwd, env.RUR_STATE_DIR);

		const started = manual ? startManualService(executable, directory, env) : null;
		try {
			if (started) await started.listening;
			const pid = await warmService(ree, directory, [], started?.service.pid);
			assert(pid, "a changed worker environment must select a new service");
			const jobCounts = serviceJobCounts(ree, directory);

			// the served runs: the worker sees what the direct one saw
			for (let job = 0; job < 2; job++) {
				const output = await servedRun(reeInProject, animationJob(directory, ++id, true), pid);
				assert.equal(JSON.parse(output).errors, 0, output);
				const served = requests().at(-1);
				for (const field of WORKER_VIEW_FIELDS) assert.equal(served[field], direct[field], field);
			}
			const jobCountsAfter = serviceJobCounts(ree, directory);
			assert.equal(jobCountsAfter.get(pid), jobCounts.get(pid) + 2, "the expected service answered both jobs");
			assert.equal(jobCountsAfter.size, manual ? 1 : servicesAfter, "each environment gets its own service");
		} finally {
			if (started) {
				await stopServices(reeInProject);
				await started.closed;
			}
		}
	}
});

// The worker names the file it wrote by its real path while the state dir is reached through a link. That is the same
// file, and copying it onto itself would fail or empty it.
check("canonical asset replies through a state link never copy a file onto itself", async ({ root, env, ree, project }) => {
	const directory = project("project");
	const real = path.join(root, "real-state");
	fs.mkdirSync(real);
	env.RUR_STATE_DIR = path.join(root, "linked-state");
	fs.symlinkSync(real, env.RUR_STATE_DIR, WINDOWS ? "junction" : "dir");
	env.REE_ASSET_WORKER = path.join(directory, "helpers", WORKER_FILE);
	env.REE_REPLY_CANONICAL = "1";
	await fetchAnimation(ree, directory, 8101);
	await warmService(ree, directory);
	await fetchAnimation(ree, directory, 8102, true);
});

check("a bare worker name uses PATH despite decoys in the project and copied service folder", async ({ root, env, ree, project, requests, executable }) => {
	const directory = project("project");
	// serve --warm reads in its project's roots
	fs.mkdirSync(path.join(directory, "root"));
	// Neither the client's current folder nor the folder of the copied service's executable (the state dir's bin) may
	// supply the worker: a decoy sits in each, and only PATH names the real one.
	const serviceBin = path.join(env.RUR_STATE_DIR, "bin");
	fs.mkdirSync(serviceBin, { recursive: true });
	for (const folder of [directory, serviceBin]) {
		const decoy = path.join(folder, WORKER_FILE);
		installWorker(decoy);
	}
	env.REE_ASSET_WORKER = WORKER_FILE;
	removePath(env);
	env.PATH = path.join(directory, "helpers");

	// a direct run, then an automatic service with a warm-up and a job through it
	await fetchAnimation(ree, directory, 1001);
	fetchAnimationFallback(ree, directory, 1002);
	const pid = await warmService(ree, directory);
	await fetchAnimation(ree, directory, 1003, true, [], pid);

	// the same through a service started by hand
	await stopServices((args) => ree(root, args));
	const service = startManualService(executable, directory, env);
	try {
		await service.listening;
		await fetchAnimation(ree, directory, 1004, true, [], service.service.pid);
		await warmService(ree, directory, [], service.service.pid);
		await fetchAnimation(ree, directory, 1005, true, [], service.service.pid);
		assert.equal(ree(directory, ["serve", "--status"]).trim().split("\n").length, 1, "clients reuse the manual service");
	} finally {
		await stopServices((args) => ree(root, args));
		await service.closed;
	}
	assert.equal(requests().length, 5);
	assert(requests().every((row) => row.worker === path.join(directory, "helpers", WORKER_FILE)), JSON.stringify(requests()));
});

// A worker installed beside the binary is named without a path. The client resolves it before a copied service moves to
// the state dir, so a direct run, an automatic service and a manual one all run the same file, with no PATH to find it.
// ".exe" is added to a name without a dot only, so each spelling must resolve to the one file.
async function findsWorkerBesideBinary(name, { env, ree, project, requests, executable }) {
	const directory = project("project");
	// serve --warm reads in its project's roots
	fs.mkdirSync(path.join(directory, "root"));
	const worker = path.join(path.dirname(executable), WORKER_FILE);
	installWorker(worker);
	removePath(env);
	env.REE_ASSET_WORKER = name;

	// a direct run, a first-call fallback, then a warm-up and a served job
	await fetchAnimation(ree, directory, 1401);
	fetchAnimationFallback(ree, directory, 1402);
	await warmService(ree, directory);
	await fetchAnimation(ree, directory, 1403, true);

	// stop that service, so the manual one is the service the clients find
	await stopServices((args) => ree(directory, args));
	const service = startManualService(executable, directory, env);
	try {
		await service.listening;
		await fetchAnimation(ree, directory, 1404, true, [], service.service.pid);
		await warmService(ree, directory, [], service.service.pid);
		assert.equal(ree(directory, ["serve", "--status"]).trim().split("\n").length, 1, "clients reuse the manual service");
	} finally {
		await stopServices((args) => ree(directory, args));
		await service.closed;
	}

	const rows = requests();
	assert.equal(rows.length, 4);
	// Windows paths are case-insensitive, and the name may be spelled in any case
	assert(rows.every((row) => row.worker.toLowerCase() === worker.toLowerCase()), JSON.stringify(rows));
}

if (WINDOWS) {
	for (const name of ["worker", "worker.exe", "worker.EXE"]) {
		check(`a worker beside the installed binary is found with PATH unset, name=${name}`, (context) => findsWorkerBesideBinary(name, context));
	}
}

check("direct, manual and warm workers resolve helper PATH and user folders", async ({ root, env, ree, project, requests, executable }) => {
	const directory = project("project");
	const helper = path.join(directory, "helpers", WINDOWS ? "helper.exe" : "helper");
	installWorker(helper);
	env.REE_WORKER_USES_HELPER = "1";
	env.REE_ASSET_WORKER = `.${path.sep}helpers${path.sep}${WORKER_FILE}`;
	// relative spellings: the worker runs in the state dir, so it only finds its helper if ree made them absolute
	env.PATH = ["helpers", env.PATH].join(path.delimiter);
	// relative spellings again: each variable must reach the worker resolved against the client's folder (the project)
	for (const name of USER_FOLDER_VARIABLES) env[name] = name.toLowerCase();
	fs.mkdirSync(path.join(directory, "root"));
	// one direct run, then a warm service and two served runs, then a manually started service and two more served runs
	await fetchAnimation(ree, directory, 1201);
	await warmService(ree, directory);
	await fetchAnimation(ree, directory, 1202, true);
	await fetchAnimation(ree, directory, 1203, true);
	await stopServices((args) => ree(root, args));
	const service = startManualService(executable, directory, env);
	try {
		await service.listening;
		await fetchAnimation(ree, directory, 1204, true, [], service.service.pid);
		await warmService(ree, directory, [], service.service.pid);
		await fetchAnimation(ree, directory, 1205, true, [], service.service.pid);
	} finally {
		await stopServices((args) => ree(root, args));
		await service.closed;
	}
	const rows = requests();
	assert.equal(rows.length, 5);
	assert(rows.every((row) => row.pathEnv.split(path.delimiter)[0] === path.join(directory, "helpers")), JSON.stringify(rows));
	for (const name of USER_FOLDER_VARIABLES) {
		assert(rows.every((row) => row.userFolders[name] === path.join(directory, env[name])), `${name}: ${JSON.stringify(rows)}`);
	}
});

if (WINDOWS) {
	for (const kind of ["drive-relative", "drive-root", "verbatim-root"]) {
		check(`Windows ${kind} environment paths keep their client meaning`, async ({ root, env, ree, project, requests, executable }) => {
			const directory = project("project");
			const driveRoot = path.parse(directory).root;
			const drive = driveRoot.slice(0, -1);
			// `C:` alone is the client's current folder on that drive, which is the project's; the others are the drive's root
			const { value, expected } = {
				"drive-relative": { value: drive, expected: directory },
				"drive-root": { value: driveRoot, expected: driveRoot },
				"verbatim-root": { value: path.toNamespacedPath(driveRoot), expected: driveRoot },
			}[kind];
			// The explicit state stays in the test folder; these values must only be forwarded to the worker.
			for (const name of USER_FOLDER_VARIABLES) env[name] = value;
			env.PATH = [value, env.PATH].join(path.delimiter);
			env.NODE_PATH = value;
			env.HOMEDRIVE = drive;
			env.SystemDrive = drive;
			env.REE_ASSET_WORKER = path.join(directory, "helpers", WORKER_FILE);
			fs.mkdirSync(path.join(directory, "root"));
			await fetchAnimation(ree, directory, 1251);
			await warmService(ree, directory);
			await fetchAnimation(ree, directory, 1252, true);
			await stopServices((args) => ree(root, args));
			const service = startManualService(executable, directory, env);
			try {
				await service.listening;
				await fetchAnimation(ree, directory, 1253, true, [], service.service.pid);
				await warmService(ree, directory, [], service.service.pid);
			} finally {
				await stopServices((args) => ree(root, args));
				await service.closed;
			}
			const rows = requests();
			assert.equal(rows.length, 3);
			for (const row of rows) {
				const firstPathEntry = row.pathEnv.split(path.delimiter)[0];
				for (const name of USER_FOLDER_VARIABLES) assert.equal(realPath(row.userFolders[name]), expected, name);
				assert.equal(realPath(firstPathEntry), expected);
				assert.equal(realPath(row.nodePath), expected);
				assert.deepEqual(row.drives, { HOMEDRIVE: drive, SystemDrive: drive });
			}
		});
	}
}

// An empty PATH entry names the client's current folder on Unix (Windows does not search it), at any position in the
// list. Two clients in different folders have different PATHs after resolving it, so each gets its own service and runs
// the worker in its own folder.
if (!WINDOWS) {
	for (const emptyAt of [0, 1, 2]) {
		check(`empty PATH entry resolves in the client at position ${emptyAt}`, async ({ env, ree, project, requests }) => {
			const first = project("first");
			const second = project("second");
			for (const directory of [first, second]) {
				installWorker(path.join(directory, WORKER_FILE));
				fs.mkdirSync(path.join(directory, "root"));
			}
			env.REE_ASSET_WORKER = WORKER_FILE;
			const entries = ["missing-first", "missing-second"];
			entries.splice(emptyAt, 0, "");
			env.PATH = entries.join(path.delimiter);
			await fetchAnimation(ree, first, 1301);
			await warmService(ree, first);
			await fetchAnimation(ree, first, 1302, true);
			const secondPid = await warmService(ree, second);
			await fetchAnimation(ree, second, 1303, true, [], secondPid);
			await fetchAnimation(ree, second, 1304, true, [], secondPid);
			const rows = requests();
			assert.equal(rows.length, 4);
			assert(rows.slice(0, 2).every((row) => row.worker === path.join(first, WORKER_FILE)), JSON.stringify(rows));
			assert(rows.slice(2).every((row) => row.worker === path.join(second, WORKER_FILE)), JSON.stringify(rows));
			assert.equal(ree(first, ["serve", "--status"]).trim().split("\n").length, 2, "resolved PATH separates client services");
		});
	}
}

// The file modes that make a worker unrunnable for the caller. Windows has no execute bit. Root can run a file when any
// execute bit is set, so mode 001 (only others may execute) is not a case for root.
function unrunnableModes() {
	if (WINDOWS) return [];
	return process.getuid() === 0 ? [0o644] : [0o644, 0o001];
}

// The lookup is the platform's own (execvp(3) on Linux and macOS), so it goes by the caller's permissions: a file the
// caller cannot execute fails with EACCES, is skipped, and the search goes on to the next PATH entry.
for (const mode of unrunnableModes()) {
	check(`worker lookup skips a file the caller cannot execute, mode=${mode.toString(8)}`, async ({ env, ree, project, requests }) => {
		const directory = project("project");
		const notExecutable = path.join(directory, "decoys");
		fs.mkdirSync(notExecutable);
		installWorker(path.join(notExecutable, WORKER_FILE), mode);
		env.REE_ASSET_WORKER = WORKER_FILE;
		env.PATH = [notExecutable, path.join(directory, "helpers"), env.PATH].join(path.delimiter);
		// one direct run, an explicit first-call fallback, then a job the service must answer
		await fetchAnimation(ree, directory, 1101);
		fetchAnimationFallback(ree, directory, 1102);
		await fetchAnimation(ree, directory, 1103, true);
		assert.equal(requests().length, 3);
		assert(requests().every((row) => row.worker === path.join(directory, "helpers", WORKER_FILE)));
	});
}

// Marks `root` as a ree checkout, so a copy of the binary in it is not a standalone run
function makeCheckout(root) {
	fs.writeFileSync(path.join(root, "Cargo.lock"), "");
	fs.writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "roblox-engine-emulator"\n');
}

// A binary in its own checkout is a host, so it starts its own worker as it did before standalone binaries: the
// worker variable is passed on as it is and a missing worker fails at spawn.
check("a missing host worker keeps its spawn error in a checkout", async ({ root, env, ree, project }) => {
	const directory = project("project");
	makeCheckout(root);
	env[HOST_WORKER_VARIABLE] = "ree_missing_host_worker";
	// Host lookup ignores the standalone failure channel, just as it ignores client-side resolution.
	env[`${HOST_WORKER_VARIABLE}_ERROR`] = "standalone lookup must not reach this host";
	fs.writeFileSync(path.join(directory, "Harness.luau"), `return function()
game:GetService("KeyframeSequenceProvider"):GetKeyframeSequenceAsync("rbxassetid://5101")
end`);
	const args = ["run", "--project", directory, "--harness", "Harness.luau", "--seconds", "0.05"];
	const direct = JSON.parse(ree(directory, args));
	assert.equal(direct.errors, 1, JSON.stringify(direct));
	assert.match(direct.failure, /starting .*worker:/);
	// No service runs yet, so the first --service call may run in the client; the next must forward the same failure
	// through the service.
	const serviceArgs = [...args, "--service"];
	const firstServiceFallback = ree(directory, serviceArgs);
	const servedReply = await servedRun(inDirectory(ree, directory), serviceArgs);
	for (const output of [firstServiceFallback, servedReply]) {
		const served = JSON.parse(output);
		assert.equal(served.failure, direct.failure, JSON.stringify(served));
		assert.equal(served.errors, direct.errors);
	}
	assert(ree(root, ["serve", "--status"]).trim(), "the service stays running after the failed job");
});

check("an unresolved worker stays unresolved when a decoy exists in the service directory", async ({ root, env, ree, project }) => {
	const directory = project("project");
	// The name is on no PATH, but a worker of that name sits in the state dir's bin, beside the copied service's executable
	env.REE_ASSET_WORKER = WINDOWS ? "only-in-state.exe" : "only-in-state";
	const decoy = path.join(env.RUR_STATE_DIR, "bin", env.REE_ASSET_WORKER);
	fs.mkdirSync(path.dirname(decoy), { recursive: true });
	installWorker(decoy);
	fs.writeFileSync(path.join(directory, "Harness.luau"), `return function()
game:GetService("KeyframeSequenceProvider"):GetKeyframeSequenceAsync("rbxassetid://5001")
end`);
	const args = ["run", "--harness", "Harness.luau", "--seconds", "0.05"];
	const direct = JSON.parse(ree(directory, args));
	assert.match(direct.failure, WINDOWS ? /asset worker .*program not found/ : /starting .*worker:/);
	// No service runs yet, so the first --service call may run in the client; the next must forward the client's lookup
	// failure through the service.
	const serviceArgs = [...args, "--service"];
	const firstServiceFallback = ree(directory, serviceArgs);
	const servedReply = await servedRun(inDirectory(ree, directory), serviceArgs);
	for (const output of [firstServiceFallback, servedReply]) {
		const served = JSON.parse(output);
		assert.equal(served.failure, direct.failure, JSON.stringify(served));
		assert.equal(served.errors, direct.errors);
	}
	assert(!fs.existsSync(env.REE_WORKER_LOG), "the decoy worker must never run");
	assert(ree(root, ["serve", "--status"]).trim(), "the service stays running after the failed job");
});

check("relative workers belong to their client, including the service environment key", async ({ env, ree, project, requests }) => {
	const first = project("first");
	const second = project("second");
	env.REE_ASSET_WORKER = `.${path.sep}helpers${path.sep}${WORKER_FILE}`;
	// each client may fall back on its first call, then its own service must answer
	await fetchAnimation(ree, first, 2001);
	fetchAnimationFallback(ree, first, 2002);
	await fetchAnimation(ree, first, 2003, true);
	const firstPid = [...serviceJobCounts(ree, first).keys()][0];
	// the second client names a different worker with the same text, so it must get its own service, not the first's
	fetchAnimationFallback(ree, second, 2004);
	const secondJob = animationJob(second, 2005, true);
	assertAnimation(await servedRun(inDirectory(ree, second), secondJob, undefined, { newService: true }));
	const pids = [...serviceJobCounts(ree, second).keys()];
	assert.equal(pids.length, 2, "each relative worker gets its own service");
	assert(pids.includes(firstPid));
	const rows = requests();
	assert.equal(rows.length, 5);
	assert(rows.slice(0, 3).every((row) => row.worker.startsWith(first)), JSON.stringify(rows));
	assert(rows.slice(3).every((row) => row.worker.startsWith(second)), JSON.stringify(rows));
});

check("relative state directories send absolute asset output paths", async ({ env, ree, project, requests }) => {
	const directory = project("project");
	env.REE_ASSET_WORKER = path.join(directory, "helpers", WORKER_FILE);
	env.RUR_STATE_DIR = "cache";
	try {
		await fetchAnimation(ree, directory, 3001);
		fetchAnimationFallback(ree, directory, 3002);
		await fetchAnimation(ree, directory, 3003, true);
		assert(requests().every((row) => path.isAbsolute(row.out)));
		// a relative path used from inside the state dir would land in cache/cache
		assert(!fs.existsSync(path.join(directory, "cache/cache")));
	} finally {
		// the state dir is relative to the project here, so the check's own stop (from its root) would miss this service
		await stopServices((args) => ree(directory, args));
	}
});

for (const service of [false, true]) check(`relative asset replies are read from the worker directory, service=${service}`, async ({ env, ree, project, requests }) => {
	const directory = project("project");
	env.REE_ASSET_WORKER = path.join(directory, "helpers", WORKER_FILE);
	env.REE_REPLY_RELATIVE = "1";
	if (service) {
		fs.mkdirSync(path.join(directory, "root"));
		await warmService(ree, directory);
	}
	await fetchAnimation(ree, directory, 7001, service);
	await fetchAnimation(ree, directory, 7002, service);
	assert.equal(requests().length, 2);
	// the worker ran in the state dir, which is where its relative replies are read from
	assert(requests().every((row) => realPath(row.cwd) === realPath(env.RUR_STATE_DIR)));
});

// Names the client's worker by a relative path, and makes any lookup through the host's variable fail, so a job
// that wrongly took the host path would show. The public build's host variable is REE_ASSET_WORKER itself, so the
// real worker is named after it.
function useRelativeWorker(env) {
	env[HOST_WORKER_VARIABLE] = "ree_missing_host_worker";
	env.REE_ASSET_WORKER = `.${path.sep}helpers${path.sep}${WORKER_FILE}`;
	env.REE_REPLY_RELATIVE = "1";
}

for (const warm of [false, true]) check(`an input repo keeps the standalone worker and service, warm=${warm}`, async ({ root, env, ree, project, requests }) => {
	const directory = project("project");
	project("state"); // a decoy worker in the service folder must never replace the client's
	const inputs = path.join(root, "inputs");
	fs.mkdirSync(inputs);
	useRelativeWorker(env);
	fs.mkdirSync(path.join(directory, "root"));
	const pid = await warmService(ree, directory);
	const job = ["--repo", "", "--repo", inputs, "--project", directory];
	await fetchAnimation(ree, directory, 7101, false, job);
	if (warm) await warmService(ree, directory, job, pid);
	await fetchAnimation(ree, directory, 7102, true, job, pid);
	await fetchAnimation(ree, directory, 7103, true, job, pid);
	const rows = requests();
	const expected = path.join(directory, "helpers", WORKER_FILE);
	assert.equal(rows.length, 3);
	assert(rows.every((row) => row.workerEnv === expected && realPath(row.worker) === realPath(expected)), JSON.stringify(rows));
	assert(rows.every((row) => path.isAbsolute(row.out) && realPath(row.cwd) === realPath(env.RUR_STATE_DIR)));
	assert(!fs.existsSync(path.join(inputs, ".cache")));
	assert.equal(ree(root, ["serve", "--status"]).trim().split("\n").length, 1, "--repo and --warm find the same service");
});

// POSTs `data` as JSON to a service and resolves with its JSON reply; anything but a 200 rejects
function postJson(port, endpoint, data) {
	return new Promise((resolve, reject) => {
		const body = JSON.stringify(data);
		const headers = { "X-REE": "1", "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) };
		const request = http.request({ hostname: "127.0.0.1", port, path: endpoint, method: "POST", headers }, (response) => {
			let text = "";
			response.on("data", (chunk) => text += chunk);
			response.on("end", () => {
				try {
					assert.equal(response.statusCode, 200, text);
					resolve(JSON.parse(text));
				} catch (error) {
					reject(error);
				}
			});
		});
		request.setTimeout(30_000, () => request.destroy(new Error("service request timed out")));
		request.on("error", reject);
		request.end(body);
	});
}

// Manual services use the same key and worker resolution as clients, including --repo and --warm.
for (const stateFlag of [false, true]) check(`a manual service is found by standalone run and warm clients with an input repo, stateFlag=${stateFlag}`, async ({ root, env, ree, project, requests, executable }) => {
	const caller = project("caller");
	project("state"); // a decoy worker in the service folder must never replace the client's
	const inputs = path.join(root, "inputs");
	fs.mkdirSync(inputs);
	useRelativeWorker(env);
	const source = fs.readFileSync(new URL("../src/project/model_assets.rs", import.meta.url), "utf8");
	// a relative path: the client's folder, not the service's state dir, must receive the dump
	env.RUR_ENGINE_DUMP = "dump.txt";
	const manifest = path.join(caller, source.match(/^pub const MANIFEST: &str = "([^"]+)"/m)[1]);
	fs.mkdirSync(path.dirname(manifest), { recursive: true });
	fs.mkdirSync(path.join(caller, "root"));
	const stateArgs = stateFlag ? ["--state-dir", path.relative(caller, env.RUR_STATE_DIR)] : [];
	const serviceEnv = { ...env };
	if (stateFlag) delete serviceEnv.RUR_STATE_DIR; // only the flag chooses this service's state
	const { service, closed, listening } = startManualService(executable, caller, serviceEnv, stateArgs);
	let port;
	try {
		port = await listening;
		const job = ["--repo", inputs, "--project", caller];
		await fetchAnimation(ree, caller, 7201, false, job);
		// the direct job wrote the dump; remove it so the job through the manual service has to write it again
		fs.unlinkSync(path.join(caller, "dump.txt"));
		await fetchAnimation(ree, caller, 7202, true, job, service.pid);
		assert(fs.existsSync(path.join(caller, "dump.txt")), "the manual service uses the client's dump path");
		assert(!fs.existsSync(path.join(env.RUR_STATE_DIR, "dump.txt")));
		// The manifest makes --warm read its catalog through normal CLI discovery.
		fs.writeFileSync(manifest, 'return { ["Models/Props"] = { AssetId = 7203, Version = 1 } }');
		await warmService(ree, caller, [...job, "--assets-at", "ReplicatedStorage.Bundles", "--assets", "*"], service.pid);
		await fetchAnimation(ree, caller, 7204, true, job, service.pid);
		const rows = requests();
		const expected = path.join(caller, "helpers", WORKER_FILE);
		assert(rows.some((row) => row.command === "catalog"), JSON.stringify(rows));
		assert(rows.every((row) => row.workerEnv === expected && realPath(row.worker) === realPath(expected)), JSON.stringify(rows));
		assert(rows.filter((row) => row.command === "asset").every((row) => path.isAbsolute(row.out)));
		assert(rows.every((row) => realPath(row.cwd) === realPath(env.RUR_STATE_DIR)));
		const services = ree(caller, ["serve", "--status"]).trim().split("\n").map(JSON.parse);
		assert.equal(services.length, 1, "--warm must reuse the manual service");
		assert.equal(services[0].pid, service.pid, "run and warm discover the service started by hand");
		assert(!fs.existsSync(path.join(inputs, ".cache")));
	} finally {
		// Ask the service to shut down and wait until its process and pipes are closed, so it no longer holds the folder.
		// One that never listened is killed. A clean shutdown exits with 0; a killed one has no exit code.
		try {
			if (port) await postJson(port, "/shutdown", {});
		} finally {
			if (!port) service.kill();
			assert.equal(await closed, port ? 0 : null);
		}
	}
});

// A bundle reply is imported from the output ree asked for, in the state dir, however the worker reports it: relative to
// its folder, or as a cached file of its own that it may not have copied to `out`. Converting the bundle writes a plan
// beside the imported model, so the plan must land in state too and never beside the worker's cache file.
async function bundleRepliesLoad({ root, env, ree, project, requests }, { cached, relative, service }) {
	const directory = project("project");
	env.REE_ASSET_WORKER = path.join(directory, "helpers", WORKER_FILE);
	env.RUR_STATE_DIR = "cache";
	if (relative) env.REE_REPLY_RELATIVE = "1";
	if (cached) {
		// the worker's own cached copy, in a folder the test can check for stray writes
		const untouched = path.join(root, "untouched");
		fs.mkdirSync(untouched);
		env.REE_REPLY_CACHED = path.join(untouched, "cached.rbxm");
		fs.writeFileSync(env.REE_REPLY_CACHED, MODEL_XML);
	}

	// the project pins one bundle in its manifest, whose path differs between this tree and the public export
	const source = fs.readFileSync(new URL("../src/project/model_assets.rs", import.meta.url), "utf8");
	const manifest = path.join(directory, source.match(/^pub const MANIFEST: &str = "([^"]+)"/m)[1]);
	fs.mkdirSync(path.dirname(manifest), { recursive: true });
	fs.writeFileSync(manifest, 'return {\n\t["Models/Props"] = { AssetId = 4001, Version = 1 },\n}');
	fs.mkdirSync(path.join(directory, "root"));
	fs.writeFileSync(path.join(directory, "Harness.luau"), 'return function() assert(game.ReplicatedStorage.Bundles.Models.Props.ClassName == "KeyframeSequence") print("worker bundle ok") end');

	try {
		let pid;
		for (let attempt = 0; attempt < 2; attempt++) {
			const environmentChanges = cached && attempt === 1;
			// the second job's worker writes nothing at `out`, so ree has to copy the cached file there
			if (environmentChanges) env.REE_SKIP_OUTPUT = "1";
			// The worker's environment is part of the service key, so a changed one needs a new service, and that one is
			// warmed. Its PID is kept so that both the warm-up and the bundle job are held to the same process.
			if (service) pid = await warmService(ree, directory, [], environmentChanges ? undefined : pid);
			// A fresh asset version makes every run read a worker reply rather than a cached bundle
			fs.writeFileSync(manifest, `return {\n\t["Models/Props"] = { AssetId = ${4001 + attempt}, Version = 1 },\n}`);
			const viaService = service ? ["--service"] : [];
			const job = ["run", "--harness", "Harness.luau", "--roots", "root", "--assets-at", "ReplicatedStorage.Bundles", "--assets", "*", "--seconds", "0.05", ...viaService];
			const output = service ? await servedRun(inDirectory(ree, directory), job, pid) : ree(directory, job);
			assert.equal(JSON.parse(output).errors, 0, output);
			assert.match(output, /worker bundle ok/);
		}
		const assets = requests().filter((row) => row.command === "asset");
		assert.equal(assets.length, 2);
		assert(assets.every((row) => path.isAbsolute(row.out)));
		assert(!fs.existsSync(path.join(directory, "cache/cache")));
		if (cached) {
			assert.deepEqual(fs.readdirSync(path.dirname(env.REE_REPLY_CACHED)), ["cached.rbxm"], "no plan or temporary write beside the reply");
			for (const row of assets) {
				assert(fs.existsSync(row.out), "the model is kept at the requested state path");
				assert(fs.existsSync(row.out + ".plan"), "conversion writes its plan in state");
			}
		}
	} finally {
		await stopServices((args) => ree(directory, args));
	}
}

for (const cached of [false, true]) {
	for (const relative of [false, true]) {
		for (const service of [false, true]) {
			check(`bundle replies load, cached=${cached}, relative=${relative}, service=${service}`, (context) => bundleRepliesLoad(context, { cached, relative, service }));
		}
	}
}

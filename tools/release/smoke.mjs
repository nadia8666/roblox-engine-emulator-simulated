// The release smoke test: ree is installed the way a user gets it, outside any checkout, and run with the per-user
// folders redirected into a temporary one. It needs no GPU, Studio, credentials or network.
//
//   node tools/release/smoke.mjs --archive <zip> [--version <version>]    a release archive, as the workflow does
//   node tools/release/smoke.mjs --binary <ree> [--version <version>]     a built binary alone (cargo test does this)
//
// It runs `--version`, an offline harness, a recording, and the same harness through the service ree starts from a
// copy in its state dir. Afterwards only the cache, the output and the temp folders may have changed, and on
// GitHub Actions the checkout the inputs came from must be untouched. Failed checks keep their folder for inspection.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { deflateSync } from "node:zlib";
import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { ROOT, PLATFORMS, args, command, isMain, main, version } from "./common.mjs";
import { checkArchive, crc32 } from "./package.mjs";

// Environment variables with these prefixes steer ree and its helpers, so a run starts without them
const STEERING_VARIABLES = /^(RUR_|REE_|ROR_)/i;
// The two ree keeps: no watching of a parent process, and a service that runs its jobs in worker processes
const KEPT_VARIABLES = ["REE_PARENT_WATCH", "REE_SERVE_WORKERS"];

// The SHA-256 of every file below `directory` (by relative path), without the folders in `ignored`
function fileHashes(directory, ignored = []) {
	const hashes = {};
	function walk(folder) {
		if (!fs.existsSync(folder)) return;
		for (const entry of fs.readdirSync(folder, { withFileTypes: true })) {
			const file = path.join(folder, entry.name);
			if (ignored.includes(file)) continue;
			if (entry.isDirectory()) walk(file);
			else if (entry.isFile()) hashes[path.relative(directory, file)] = createHash("sha256").update(fs.readFileSync(file)).digest("hex");
		}
	}
	walk(directory);
	return hashes;
}

// A 1x1 RGBA PNG for the harness to read as content relative to the project
export function pixelPng() {
	const chunk = (type, data) => {
		const body = Buffer.concat([Buffer.from(type), data]);
		const length = Buffer.alloc(4);
		const checksum = Buffer.alloc(4);
		length.writeUInt32BE(data.length);
		checksum.writeUInt32BE(crc32(body));
		return Buffer.concat([length, body, checksum]);
	};
	const header = Buffer.alloc(13);
	header.writeUInt32BE(1, 0); // width
	header.writeUInt32BE(1, 4); // height
	header[8] = 8; // bits per channel
	header[9] = 6; // RGBA
	const row = Buffer.from([0, 0, 0, 0, 255]); // filter byte 0, then red, green, blue and alpha of the one pixel
	const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
	return Buffer.concat([signature, chunk("IHDR", header), chunk("IDAT", deflateSync(row)), chunk("IEND", Buffer.alloc(0))]);
}

function thisPlatform() {
	if (process.platform === "win32") return PLATFORMS.windows;
	if (process.platform === "darwin") return PLATFORMS.macos;
	return PLATFORMS.linux;
}

// Puts ree into `bin`: every file of the archive, or the one binary
function install(options, bin, platform) {
	const executable = path.join(bin, platform.exe);
	if (options.binary) {
		fs.copyFileSync(options.binary, executable);
		fs.chmodSync(executable, 0o755);
		return executable;
	}
	for (const entry of checkArchive(fs.readFileSync(options.archive), platform.exe)) {
		const file = path.join(bin, entry.name);
		fs.writeFileSync(file, entry.bytes);
		fs.chmodSync(file, entry.mode & 0o777);
	}
	return executable;
}

// Files ree must not pick up: the working folder and the executable's folder supply no implicit inputs
function plantDecoys(folders) {
	for (const folder of folders) {
		fs.writeFileSync(path.join(folder, ".env"), "REE_RELEASE_DECOY=must-not-be-read\n");
		fs.mkdirSync(path.join(folder, ".cache/snapshots/TESTING"), { recursive: true });
		fs.writeFileSync(path.join(folder, ".cache/snapshots/TESTING/invalid.rbxm"), "not a snapshot");
	}
}

// Whether a process with this id is still alive (signal 0 only checks)
function isRunning(pid) {
	try {
		process.kill(pid, 0);
		return true;
	} catch (error) {
		if (error.code === "ESRCH") return false;
		throw error;
	}
}

// Waits until none of the processes a check started is running. A worker can outlive the ree that started it by a
// moment, and a running process holds its folder, which the check's cleanup needs. One deadline covers all of them.
export async function waitForProcesses(pids) {
	const deadline = Date.now() + 10_000;
	for (const pid of new Set(pids)) {
		while (isRunning(pid)) {
			assert(Date.now() < deadline, `process ${pid} did not stop`);
			await sleep(50);
		}
	}
}

// The errors Windows gives while it is still releasing a folder's handles after the process that used it exited
const BUSY_FOLDER_ERRORS = ["EBUSY", "EPERM", "ENOTEMPTY"];

// Removes a check's temporary folder, retrying for a few seconds while Windows still holds it. A folder that stays
// held (or any other error) is reported with the path that could not be removed and is left in place, and this returns
// false: leftovers in a temporary folder must not turn a passing check into a failing one. Returns true once removed.
export async function removeTestFolder(root) {
	const deadline = Date.now() + 10_000;
	for (;;) {
		try {
			await fs.promises.rm(root, { recursive: true, force: true });
			return true;
		} catch (error) {
			const retryable = BUSY_FOLDER_ERRORS.includes(error.code) && Date.now() < deadline;
			if (!retryable) {
				console.error(`cleanup could not remove ${error.path || root} (${error.code}); files kept at ${root}`);
				return false;
			}
			await sleep(100);
		}
	}
}

// Starts `ree serve` by hand in `cwd`. `listening` resolves with its port once it says where it listens, and rejects if
// it fails to start or exits first. `closed` resolves with the exit code once the process and its pipes are closed,
// which `exit` does not wait for, and open pipes would still hold the state folder on Windows.
export function startManualService(executable, cwd, env, args = []) {
	const service = spawn(executable, ["serve", "--workers", "1", "--idle", "30", ...args], { cwd, env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
	const closed = new Promise((resolve) => service.once("close", resolve));
	const listening = new Promise((resolve, reject) => {
		let log = "";
		const timer = setTimeout(() => reject(new Error(`service did not start: ${log}`)), 30_000);
		service.once("error", (error) => { clearTimeout(timer); reject(error); });
		service.once("close", (code) => { clearTimeout(timer); reject(new Error(`service exited ${code}: ${log}`)); });
		// nothing reads stdout, but an unread pipe would fill up and stall the service
		service.stdout.resume();
		service.stderr.on("data", (chunk) => {
			log += chunk;
			const match = log.match(/service on 127\.0\.0\.1:(\d+)/);
			if (match) { clearTimeout(timer); resolve(Number(match[1])); }
		});
	});
	return { service, closed, listening };
}

// Stops the services `ree` started. A stop request returns before a service exits, so this waits for each process to
// release its state folder, which a test or the temporary folder's removal needs.
// `ree(args)` runs ree with these arguments and returns its output.
export async function stopServices(ree) {
	const status = ree(["serve", "--status"]).trim();
	const pids = status ? status.split("\n").map((line) => JSON.parse(line).pid) : [];
	ree(["serve", "--stop"]);
	await waitForProcesses(pids);
}

// The running services, one `serve --status` line each; only the one with process id `pid` when it is given
function runningServices(ree, pid) {
	const services = ree(["serve", "--status"]).trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
	return pid === undefined ? services : services.filter((service) => service.pid === pid);
}

// Runs a --service job or a serve --warm and returns its output. A client that cannot use its service runs the job
// itself and still succeeds, so success proves nothing about where it ran. The proof is the expected service: it keeps
// its process id and its completed-job count rises by one.
// The count is polled, not read once: the service sends its reply before it counts the job, because the worker has not
// yet released the job's engine.
// With no `expectedPid`, the one running service (or the one the job starts) is the expected one. `newService` says
// the job must start a new process, as a changed environment or client folder does; services already running don't count.
export async function servedRun(ree, argv, expectedPid, { newService = false } = {}) {
	const ignoredPids = newService ? new Set(runningServices(ree).map((service) => service.pid)) : new Set();
	const expectedServices = () => runningServices(ree, expectedPid).filter((service) => !ignoredPids.has(service.pid));
	let before = expectedServices();
	assert(before.length <= 1, "name the expected service when more than one is running");
	expectedPid ??= before[0]?.pid;
	if (expectedPid !== undefined) assert.equal(before[0]?.pid, expectedPid, "the expected service is running");

	// A job still finishing from an earlier call would move the count and be taken for this one
	const isBusy = (service) => service?.health.jobs.running > 0 || service?.health.jobs.queued > 0;
	const idleDeadline = Date.now() + 10_000;
	while (isBusy(before[0])) {
		assert(Date.now() < idleDeadline, "the expected service must finish its previous jobs");
		await sleep(25);
		before = expectedServices();
		assert.equal(before.length, 1, "the expected service must stay running");
	}
	const doneBefore = before[0]?.health.jobs.done ?? 0;

	const printed = ree(argv);

	const deadline = Date.now() + 10_000;
	for (;;) {
		const after = expectedServices();
		assert.equal(after.length, 1, "the expected service must stay running after the job");
		expectedPid ??= after[0].pid;
		assert.equal(after[0].pid, expectedPid, "the expected service must answer the job");
		const doneAfter = after[0].health.jobs.done;
		if (doneAfter !== doneBefore || Date.now() >= deadline) {
			assert.equal(doneAfter, doneBefore + 1, "the expected service must complete the job without client fallback");
			return printed;
		}
		await sleep(25);
	}
}

// Asks the binary whether it was built with bundles. With no worker configured, a build with bundles asks for
// REE_ASSET_WORKER and a build without them says it downloads no assets. Neither answer starts a worker or uses the
// network.
const BUNDLES_PROBE = `return function()
local ok, message = pcall(function()
	game:GetService("KeyframeSequenceProvider"):GetKeyframeSequenceAsync("rbxassetid://1")
end)
assert(not ok, "the capability probe has no worker")
if string.find(tostring(message), "this build downloads no assets", 1, true) then
	print("bundles capability: unavailable")
else
	assert(string.find(tostring(message), "set REE_ASSET_WORKER", 1, true), tostring(message))
	print("bundles capability: available")
end
end`;

// Runs the Unix worker-lookup tests (they need an asset worker) only when the tested binary has bundles: the
// standalone test target also builds with the cli feature alone, and that binary has no worker to look up.
export function checkAssetWorker(ree, env, temp, run = command) {
	const harness = path.join(temp, "Bundles.luau");
	fs.writeFileSync(harness, BUNDLES_PROBE);
	const printed = ree(["run", "--harness", harness, "--seconds", "0.05"]);
	assert.equal(JSON.parse(printed).errors, 0, printed);
	assert.match(printed, /bundles capability: (available|unavailable)/);
	const hasBundles = printed.includes("bundles capability: available");
	if (!hasBundles) return;
	const workerTests = path.join(ROOT, "tests/asset_worker.mjs");
	run(process.execPath, ["--test", "--test-name-pattern", "worker lookup skips", workerTests], { env, timeout: 120_000 });
}

// Runs every check inside `root`, a fresh temporary folder. Throws on the first failure.
async function smoke(options, root) {
	const platform = thisPlatform();

	// everything lives in one fresh temporary folder
	const bin = path.join(root, "bin");
	const project = path.join(root, "project");
	const output = path.join(root, "output");
	const home = path.join(root, "home");
	const localAppData = path.join(root, "local");
	const xdgCache = path.join(root, "xdg");
	const temp = path.join(root, "temp");
	for (const folder of [bin, project, output, home, localAppData, xdgCache, temp]) fs.mkdirSync(folder);

	const executable = install(options, bin, platform);
	fs.copyFileSync(path.join(ROOT, "tools/release/Smoke.luau"), path.join(project, "Smoke.luau"));
	fs.writeFileSync(path.join(project, "pixel.png"), pixelPng());
	plantDecoys([project, bin]);
	// a Cargo.lock beside the project must not make it the executable's checkout
	fs.writeFileSync(path.join(project, "Cargo.lock"), "not this executable's checkout");
	// nor may an unrelated Cargo project above the installed executable (the temporary folder is its parent)
	fs.writeFileSync(path.join(root, "Cargo.lock"), "an unrelated lockfile");
	fs.writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname = "another-tool"\nversion = "0.1.0"\n');

	// ree's per-user cache folder on each platform, redirected into the temporary folder
	const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => KEPT_VARIABLES.includes(name) || !STEERING_VARIABLES.test(name)));
	Object.assign(env, {
		LOCALAPPDATA: localAppData, APPDATA: path.join(root, "roaming"), HOME: home, USERPROFILE: home, XDG_CACHE_HOME: xdgCache,
		TMPDIR: temp, TMP: temp, TEMP: temp, REE_PARENT_WATCH: "0", REE_SERVE_WORKERS: "1",
	});
	let state = path.join(xdgCache, "ree");
	if (process.platform === "win32") state = path.join(localAppData, "ree");
	if (process.platform === "darwin") state = path.join(home, "Library/Caches/ree");

	// the folders that may change; on Actions also the checkout the inputs came from
	const allowedChanges = [output, state, temp];
	const filesBefore = fileHashes(root, allowedChanges);
	const workspace = process.env.GITHUB_WORKSPACE;
	const workspaceBefore = workspace ? fileHashes(workspace) : null;

	const ree = (argv) => command(executable, argv, { cwd: project, env, timeout: 120_000 });

	// Runs the harness; it must print its line and report an ok status without errors. `run` prints the script's
	// output, `record` writes it to events.json. `served` says the service must complete the job (see servedRun). It is
	// given by the caller, not read from the arguments, so a client that stopped sending --service would still fail.
	const expectHarnessRun = async (argv, served = false) => {
		const printed = served ? await servedRun(ree, argv) : ree(argv);
		const result = JSON.parse(printed.trim());
		const scriptOutput = argv[0] === "record" ? fs.readFileSync(path.join(output, "events.json"), "utf8") : printed;
		assert.match(scriptOutput, /release smoke 1,2,3/);
		assert.equal(result.status, "ok", printed);
		assert.equal(result.errors, 0, printed);
	};

	let failure;
	try {
		assert.equal(ree(["--version"]).trim(), `ree ${options.version || version()}`);
		assert.throws(() => ree(["profile", "view", output]), /release installs contain only ree/);

		const harness = ["--harness", "Smoke.luau", "--seconds", "0.05", "--offline"];
		await expectHarnessRun(["run", ...harness]);
		await expectHarnessRun(["record", ...harness, "--out", output]);
		assert(fs.existsSync(path.join(output, "scene.rbxm")));
		assert(fs.readdirSync(path.join(state, "editable-images")).some((name) => name.endsWith(".png")), "image snapshots use the redirected per-user cache");

		// With no service running, the first --service call runs the job itself; the calls after it must be answered by
		// the service.
		const serviceRun = ["run", "--service", ...harness];
		await expectHarnessRun(serviceRun);
		await expectHarnessRun(serviceRun, true);
		await expectHarnessRun(serviceRun, true);
		// Empty roots parse as no roots: local content still belongs to the caller's project, also in the service.
		for (const roots of [["--roots", ""], ["--roots", ";;"], ["--roots", "unused", "--roots", ""]]) {
			await expectHarnessRun(["run", ...harness, ...roots]);
			await expectHarnessRun(["run", "--service", ...harness, ...roots], true);
		}
		assert.match(ree(["serve", "--status"]), /"port":/, "the copied service runs in the same standalone state");
		if (process.platform !== "win32") {
			// Linux and macOS only: the worker lookup must skip a PATH entry without execute permission, as the OS does.
			// checkAssetWorker runs that test only for a binary built with bundles.
			checkAssetWorker(ree, { ...env, REE_TEST_BINARY: executable }, temp);
		}
		// A relative XDG_CACHE_HOME is ignored, so the cache falls back to HOME/.cache/ree. Check that on the release
		// runner for direct jobs, automatically started services, a service started by hand, and warm-up.
		command(process.execPath, ["--test", "--test-name-pattern", "relative XDG_CACHE_HOME", path.join(ROOT, "tests/service_cache.mjs")], {
			env: { ...env, REE_TEST_BINARY: executable }, timeout: 120_000,
		});
	} catch (error) {
		failure = error;
	} finally {
		// stop the service even when a check failed, but report the first failure
		try {
			await stopServices(ree);
		} catch (error) {
			failure ||= error;
		}
	}
	if (failure) throw failure;

	assert.deepEqual(fileHashes(root, allowedChanges), filesBefore, "writes outside the redirected cache, output and temp folders");
	if (workspace) assert.deepEqual(fileHashes(workspace), workspaceBefore, "the input checkout was modified");
}

if (isMain(import.meta.url)) main(async () => {
	const options = args(process.argv.slice(2), ["archive", "binary", "version"]);
	if (Boolean(options.archive) === Boolean(options.binary)) {
		throw new Error("usage: smoke.mjs --archive <zip> | --binary <ree> [--version <version>]");
	}
	// the real path of the temp folder, so the paths the smoke builds match the ones the binary reports
	const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "ree-release-smoke-")));
	// A passing run removes its folder; a failing one keeps it for inspection and says where it is
	try {
		await smoke(options, root);
		await removeTestFolder(root);
	} catch (error) {
		console.error(`release smoke failed; files kept at ${root}`);
		throw error;
	}
	console.log("release smoke passed");
});

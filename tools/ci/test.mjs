// Tests of the CI tools (the test runner, its entry and the compile cache) and of what the workflow must keep:
// node --test tools/ci/test.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { command } from "../release/common.mjs";
import { cacheKey, checked, download, parseEnvironment, stop } from "./compile-cache.mjs";
import { OWN_JOBS, jobOf, parseTestList, partition, run } from "./test-runner.mjs";

const WORKFLOW = fs.readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");

// Where cargo runs planted's binary (the profile-view package's folder), and the package name it sets there
const cwd = path.resolve("profile-view");
const env = { CARGO_PKG_NAME: "ree-profile-view" };
const planted = path.resolve("target/release/deps/planted-0123abcd.exe");
// Standalone's binary belongs to the engine's package, which cargo runs in the repository's root
const standalone = path.resolve("target/release/deps/standalone-0123abcd.exe");
const engineEnv = { CARGO_PKG_NAME: "roblox-engine-emulator" };
// A folder that is also named profile-view, but belongs to another package
const otherPackage = path.resolve("tools/profile-view");

// A fake spawn that answers every call with `result` and records the arguments it was called with
function fakeSpawn(result) {
	const calls = [];
	const spawn = (file, args) => {
		calls.push(args);
		return result;
	};
	return { calls, spawn };
}

test("the slices of the live list hold every test exactly once, for new and similar names too", () => {
	const names = parseTestList("a::first: test\r\na::first_extended: test\r\nz: test\r\nnew_test: test\r\n");
	for (const count of [1, 2, 3, 4, 7]) {
		const slices = Array.from({ length: count }, (_, index) => partition(names, index, count));
		assert.deepEqual(slices.flat().sort(), names);
		assert.equal(new Set(slices.flat()).size, names.length);
	}
});

test("a list that is empty, repeats a name or does not read as libtest's fails, and so does a bad shard", () => {
	for (const text of ["", "a: test\na: test", "a: test\nunknown", "--skip: test"]) assert.throws(() => parseTestList(text));
	for (const [index, count] of [[-1, 3], [3, 3], [0, 0], [0.5, 3], [0, NaN]]) assert.throws(() => partition([], index, count));
});

test("core skips only the two test binaries with jobs of their own and passes everything else through", () => {
	assert.equal(jobOf(planted, "ree-profile-view"), "planted");
	assert.equal(jobOf(standalone, "roblox-engine-emulator"), "standalone");
	// the same target in another package, each other's package, no package name, and other targets of the two packages
	assert.equal(jobOf(planted, "another-package"), "core");
	assert.equal(jobOf(planted, "roblox-engine-emulator"), "core");
	assert.equal(jobOf(standalone, "ree-profile-view"), "core");
	assert.equal(jobOf(planted, undefined), "core");
	assert.equal(jobOf(standalone, undefined), "core");
	assert.equal(jobOf(path.resolve("target/release/deps/engine-0123abcd.exe"), "ree-profile-view"), "core");
	assert.equal(jobOf(path.resolve("target/release/deps/standalone_more-0123abcd.exe"), "roblox-engine-emulator"), "core");

	for (const [binary, packageEnv] of [[planted, env], [standalone, engineEnv]]) {
		const skipped = fakeSpawn({ status: 23 });
		assert.equal(run(["core", binary], { cwd, env: packageEnv, spawn: skipped.spawn, log() {} }), 0);
		assert.deepEqual(skipped.calls, []);
	}
});

test("the workflow has jobs for both test binaries that core skips", () => {
	const modes = [...WORKFLOW.matchAll(/runner=\['node','\$entry','(\w+)'/g)].map(match => match[1]);
	assert.deepEqual(modes.sort(), ["core", ...Object.keys(OWN_JOBS)].sort());
});

// The runner knows a test binary by its package and its file's name, and cargo names the file after the target alone.
// A binary, example or bench of the same package with the same name would read as the same: core would skip its tests,
// and the job of its own, which asks cargo for the test target, would never run them.
test("each test binary with a job of its own is a test target, and the only target of that name in its package", () => {
	const { packages } = JSON.parse(command("cargo", ["metadata", "--no-deps", "--format-version", "1", "--locked"]));
	for (const { packageName, target } of Object.values(OWN_JOBS)) {
		const targets = packages.find(candidate => candidate.name === packageName)?.targets ?? [];
		// in a file's name cargo writes a hyphen of the target's name as an underscore
		const kinds = targets.filter(candidate => candidate.name.replaceAll("-", "_") === target).map(candidate => candidate.kind);
		assert.deepEqual(kinds, [["test"]], `the targets of ${packageName} named ${target}`);
	}
});

// A test binary named planted that is not profile-view's: core runs it and returns its exit code, a shard rejects it
function checkRunsInCoreOnly(packageEnv) {
	const passedThrough = fakeSpawn({ status: 23 });
	const options = { cwd: otherPackage, env: packageEnv, spawn: passedThrough.spawn };
	assert.equal(run(["core", planted, "--list", "--format", "terse"], options), 23);
	assert.deepEqual(passedThrough.calls, [["--list", "--format", "terse"]]);
	assert.throws(() => run(["planted", "0", "3", planted], options), /different test target/);
	assert.equal(passedThrough.calls.length, 1);
}

test("core runs the planted binary of another package in a folder also named profile-view, and a shard rejects it", () => {
	checkRunsInCoreOnly({ CARGO_PKG_NAME: "another-package" });
});

test("core runs the planted binary when cargo gives no package name, and a shard rejects it", () => {
	checkRunsInCoreOnly({});
});

test("the standalone job runs its test binary as cargo asked and rejects any other", () => {
	const ran = fakeSpawn({ status: 23 });
	const options = { cwd: path.resolve("."), env: engineEnv, spawn: ran.spawn };
	assert.equal(run(["standalone", standalone, "--list", "--format", "terse"], options), 23);
	assert.deepEqual(ran.calls, [["--list", "--format", "terse"]]);
	// planted's binary, a standalone target of another package or of no known package, and standalone's binary in a shard
	assert.throws(() => run(["standalone", planted], { ...options, env }), /different test target/);
	assert.throws(() => run(["standalone", standalone], { ...options, env: { CARGO_PKG_NAME: "another-package" } }), /different test target/);
	assert.throws(() => run(["standalone", standalone], { ...options, env: {} }), /different test target/);
	assert.throws(() => run(["planted", "0", "3", standalone], options), /different test target/);
	assert.equal(ran.calls.length, 1);
});

test("a shard lists the binary, runs its slice by exact name one test at a time and exits with the tests' code", () => {
	const calls = [];
	const spawn = (file, args, options) => {
		calls.push(args);
		assert.equal(file, planted);
		assert.equal(options.cwd, cwd);
		if (calls.length === 1) return { status: 0, stdout: "a: test\na_extra: test\nb: test\nc: test\n" };
		return { status: 17 };
	};
	assert.equal(run(["planted", "0", "3", planted], { cwd, env, spawn, log() {} }), 17);
	assert.deepEqual(calls, [["--list", "--format", "terse"], ["--exact", "--test-threads=1", "a", "c"]]);
});

test("a failed listing, a signal, a launch error or a bad argument cannot turn a shard green", () => {
	const argv = ["planted", "0", "3", planted];
	assert.equal(run(argv, { cwd, env, spawn: () => ({ status: 7 }) }), 7);
	assert.equal(run(argv, { cwd, env, spawn: () => ({ status: null, signal: "SIGTERM" }) }), 1);
	assert.throws(() => run(argv, { cwd, env, spawn: () => ({ error: new Error("cannot start") }) }), /cannot start/);
	assert.throws(() => run(argv, { cwd, env, spawn: () => ({ status: 0, stdout: "broken list" }) }), /unrecognized/);
	assert.throws(() => run(["planted", "0", "3", path.resolve("target/release/deps/engine-0123abcd.exe")], { cwd, env }), /different test target/);
	assert.throws(() => run(["unknown", planted]), /runner mode/);
	assert.throws(() => run(["core"]), /missing test binary/);
});

test("a shard with no tests runs nothing, and the arguments for the binary come before the names", () => {
	const only = fakeSpawn({ status: 0, stdout: "only: test\n" });
	assert.equal(run(["planted", "2", "3", planted], { cwd, env, spawn: only.spawn, log() {} }), 0);
	assert.equal(only.calls.length, 1);
	assert.equal(run(["planted", "0", "3", planted, "--list", "--format", "terse"], { cwd, env, spawn: only.spawn, log() {} }), 0);
	assert.deepEqual(only.calls[2], ["--exact", "--test-threads=1", "--list", "--format", "terse", "only"]);
});

// node stands in for the test binary, and the entry runs as cargo starts it, as a separate process
test("the entry runs the test binary and exits with its code, whether that is 0 or not", () => {
	const entry = fileURLToPath(new URL("./test-entry.mjs", import.meta.url));
	for (const code of [0, 23]) {
		const script = `console.log("test binary ran"); process.exitCode = ${code};`;
		const result = spawnSync(process.execPath, [entry, "core", process.execPath, "-e", script], { encoding: "utf8", windowsHide: true });
		assert.ifError(result.error);
		assert.equal(result.stdout.trim(), "test binary ran");
		assert.equal(result.stderr, "");
		assert.equal(result.status, code);
	}
});

test("a download is used only when its SHA-256 is the pinned one", () => {
	const archive = Buffer.from("the release archive");
	const sha256 = createHash("sha256").update(archive).digest("hex");
	assert.equal(checked(archive, sha256, "sccache.zip"), archive);
	assert.throws(() => checked(Buffer.from("another archive"), sha256, "sccache.zip"), /sccache\.zip: checksum mismatch/);
});

// A fake fetch that gives `answers` in turn, and the last one from then on, and records the addresses it was asked
// for. An answer that is an Error is thrown.
function fakeFetch(...answers) {
	const calls = [];
	const get = async (url) => {
		const answer = answers[Math.min(calls.length, answers.length - 1)];
		calls.push(url);
		if (answer instanceof Error) throw answer;
		return answer;
	};
	return { calls, get };
}

test("a download that fails or stalls is tried again after a growing pause, and gives up with the address and the cause", async () => {
	const archive = Buffer.from("the release archive");
	const pin = { url: "https://releases.example/sccache.zip", sha256: createHash("sha256").update(archive).digest("hex") };
	const refused = new TypeError("fetch failed", { cause: new Error("connect ECONNREFUSED 192.0.2.1:443") });
	const pauses = [];
	const options = { attempts: 4, wait: 100, pause: async (ms) => void pauses.push(ms), log() {} };

	// a network error, then a server's error, then the archive
	const third = fakeFetch(refused, new Response("busy", { status: 503 }), new Response(archive));
	assert.deepEqual(await download(pin, { ...options, get: third.get }), archive);
	assert.deepEqual(third.calls, [pin.url, pin.url, pin.url]);
	assert.deepEqual(pauses, [100, 200]);

	const never = fakeFetch(refused);
	const failure = /https:\/\/releases\.example\/sccache\.zip: no download in 4 attempts, the last one failed: fetch failed: connect ECONNREFUSED 192\.0\.2\.1:443/;
	await assert.rejects(download(pin, { ...options, get: never.get }), failure);
	assert.equal(never.calls.length, 4);
	const busy = fakeFetch(new Response("busy", { status: 503 }));
	await assert.rejects(download(pin, { ...options, attempts: 1, get: busy.get }), /no download in 1 attempts, the last one failed: HTTP 503/);

	// an answer that has not come when an attempt's time is up fails that attempt. The fake's timer keeps the process
	// waiting until then, as fetch's open connection does.
	const stalled = (url, { signal }) => new Promise((resolve, reject) => {
		const connection = setInterval(() => {}, 1000);
		signal.addEventListener("abort", () => {
			clearInterval(connection);
			reject(signal.reason);
		});
	});
	await assert.rejects(download(pin, { ...options, attempts: 2, timeout: 20, get: stalled }), /no download in 2 attempts, the last one failed: .*timeout/);
});

test("a download whose SHA-256 is not the pinned one fails at once, with no second attempt", async () => {
	const pin = { url: "https://releases.example/sccache.zip", sha256: createHash("sha256").update("the release archive").digest("hex") };
	const other = fakeFetch(new Response("another archive"));
	const options = { get: other.get, pause: () => assert.fail("it waited for another attempt"), log() {} };
	await assert.rejects(download(pin, options), /sccache\.zip: checksum mismatch/);
	assert.equal(other.calls.length, 1);
});

test("the environment script's variables are read as cmd prints them", () => {
	const printed = "INCLUDE=C:\\VC\\include;C:\\Kits\\ucrt\r\nVCToolsVersion=14.44.35207\r\nPROMPT=$P$G=\r\n\r\n";
	assert.deepEqual(parseEnvironment(printed), { INCLUDE: "C:\\VC\\include;C:\\Kits\\ucrt", VCToolsVersion: "14.44.35207", PROMPT: "$P$G=" });
});

test("the cache key follows a vendored file's bytes and name and the compilers' versions, and nothing else", (t) => {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "ree-compile-cache-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	const source = path.join(root, "vendor", "vm", "lvm.cpp");
	fs.mkdirSync(path.dirname(source), { recursive: true });
	fs.writeFileSync(source, "int a;");
	fs.writeFileSync(path.join(root, "Cargo.lock"), "version = 4");
	fs.writeFileSync(path.join(root, "README.md"), "not an input");
	const key = (compiler = "19.44.35207") => cacheKey(root, ["vendor", "Cargo.lock"], ["rustc 1.90.0", compiler]);

	const first = key();
	assert.match(first, /^compile-cache-[0-9a-f]{32}$/);
	// a file that is no input, and an input's modification time, leave the key as it is
	fs.writeFileSync(path.join(root, "README.md"), "still not an input");
	fs.utimesSync(source, new Date(0), new Date(0));
	assert.equal(key(), first);

	assert.notEqual(key("19.45.0"), first);
	fs.writeFileSync(source, "int b;");
	const edited = key();
	assert.notEqual(edited, first);
	fs.renameSync(source, path.join(root, "vendor", "vm", "lgc.cpp"));
	assert.notEqual(key(), edited);
});

test("stopping prints sccache's statistics, and a job in which no server is running is told so and passes", () => {
	// what `stop` prints when sccache answers its --stop-server with `result`
	function stopped(result) {
		const lines = [];
		const fake = fakeSpawn(result);
		stop({ env: { RUSTC_WRAPPER: "C:\\tools\\sccache.exe" }, spawn: fake.spawn, log: line => lines.push(line) });
		assert.deepEqual(fake.calls, [["--stop-server"]]);
		return lines.join("\n");
	}
	assert.match(stopped({ status: 0, stdout: "Stopping sccache server...\nCache hits   120\n", stderr: "" }), /Cache hits   120/);
	// sccache 0.19.0's answer when no server is running
	const refused = "sccache: error: couldn't connect to server\nsccache: caused by: No connection could be made because the target machine actively refused it. (os error 10061)\n";
	assert.match(stopped({ status: 2, stdout: "Stopping sccache server...\n", stderr: refused }), /^compile cache: no sccache server is running/);

	// any other failure of sccache, one that could not be started, and a job in which `start` did not run fail the step
	const unread = { status: 2, stdout: "Stopping sccache server...\n", stderr: "sccache: error: failed to read response\n" };
	assert.throws(() => stopped(unread), /--stop-server: exit 2: sccache: error: failed to read response/);
	assert.throws(() => stopped({ status: null, signal: "SIGTERM", stdout: "", stderr: "" }), /--stop-server: exit null/);
	assert.throws(() => stopped({ error: new Error("spawnSync sccache.exe ENOENT") }), /--stop-server: spawnSync sccache\.exe ENOENT/);
	assert.throws(() => stop({ env: {}, spawn: () => assert.fail("sccache was started") }), /RUSTC_WRAPPER is not set/);
});

test("every job sets up the compile cache before it restores the two caches, and stops it after its tests", () => {
	const jobs = WORKFLOW.split(/^jobs:/m)[1].split(/^  \w+:\r?\n/m).slice(1);
	assert.equal(jobs.length, 3);
	for (const job of jobs) {
		const steps = ["compile-cache.mjs start", "actions/cache/restore@", "Swatinem/rust-cache@", "cargo test", "compile-cache.mjs stop"];
		const order = steps.map(step => job.indexOf(step));
		assert(order.every(at => at >= 0), "a job is missing one of the steps");
		assert.deepEqual(order, [...order].sort((a, b) => a - b));
	}
});

test("the source checks run once, in the standalone job, before any step that a failed download or restore could skip them with", () => {
	const standaloneJob = WORKFLOW.split(/^  standalone:\r?\n/m)[1];
	const firstCacheStep = standaloneJob.indexOf("compile-cache.mjs start");
	for (const check of ["run: node tools/vendor.mjs --check", "run: node --test tools/ci/test.mjs"]) {
		assert.equal(WORKFLOW.split(check).length, 2, check);
		const at = standaloneJob.indexOf(check);
		assert(at >= 0 && at < firstCacheStep, `${check} comes before the caches`);
	}
});

test("only the test job on main stores the compile cache, and sccache never gets the workflow's token", () => {
	const stores = WORKFLOW.split(/^      - /m).filter(step => step.includes("actions/cache/save@"));
	assert.equal(stores.length, 1);
	assert.match(stores[0], /if: \$\{\{ github\.ref == 'refs\/heads\/main' && /);
	assert(WORKFLOW.split(/^  planted:/m)[0].includes("actions/cache/save@"), "the store step belongs to the test job");
	// the whole cache action would also store at the end of any job that missed, a pull request's included
	assert(!WORKFLOW.includes("uses: actions/cache@"));
	// sccache's own GitHub storage needs the runner's cache token in the environment of the build
	assert.doesNotMatch(WORKFLOW, /SCCACHE_GHA|ACTIONS_RUNTIME_TOKEN|ACTIONS_CACHE_URL|ACTIONS_RESULTS_URL/);
	// every action is pinned to a full commit SHA
	for (const line of WORKFLOW.split("\n").filter(line => line.includes("uses:"))) assert.match(line, /@[0-9a-f]{40}\b/);
});

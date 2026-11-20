// The test runner behind the CI jobs (.github/workflows/ci.yml). Cargo starts every test binary through test-entry.mjs,
// which calls run() here:
//
//   node tools/ci/test-entry.mjs core <binary> [test arguments]
//   node tools/ci/test-entry.mjs planted <index> <count> <binary> [test arguments]
//   node tools/ci/test-entry.mjs standalone <index> <count> <binary> [test arguments]
//
// Two test binaries take most of a run's testing time, so each runs in jobs of its own, beside the rest:
//
// - The planted profile tests (profile-view/tests/planted.rs): each runs the engine under the profiler, and one lock
//   around the engine keeps them from running side by side. So CI runs them on `count` machines, each running one slice,
//   one test at a time.
// - The standalone tests (tests/standalone.rs): one test runs the installed binary's checks six times in a row and
//   takes two minutes, longer than every other test of the test job together. CI runs them on `count` machines too,
//   each slice on libtest's own threads.
//
// The mode says which job this is:
//
//   core        runs every test binary as cargo asked, except planted's and standalone's, which it skips
//   planted,    run one slice of that binary's tests. The runner lists the binary's tests and deals them out to the
//   standalone  `count` slices longest first, each to the slice with the least time so far (ties to the lowest), by the
//               seconds test-weights.json holds for that mode's slow tests (any other test counts as RUN_MS). Every job
//               computes the same slices from the same list, so they cover it without overlap and every test runs in
//               exactly one job; a new test joins a slice by its name with no list to maintain here. A missing or stale
//               time only makes the jobs less even.
//
// The exit code is the test binary's, so a failing test fails the job. A list that is empty, has a duplicate or does
// not read as libtest's fails the job too, and so does a planted or standalone job that is handed another test binary,
// so that no test is lost unseen.
//
// The arguments are positional, unlike the --name value pairs of the release scripts: cargo puts the test binary and
// its arguments after the ones in its runner setting.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

// The names in libtest's `--list --format terse` output (one `name: test` per line), sorted
export function parseTestList(text) {
	const names = [];
	for (const line of text.split(/\r?\n/)) {
		if (!line.trim()) continue;
		const match = /^(.+): (?:test|benchmark)$/.exec(line);
		// a name that starts with "-" would read as an option when it is passed back to the binary
		if (!match || match[1].startsWith("-")) throw new Error(`unrecognized libtest --list row: ${line}`);
		names.push(match[1]);
	}
	if (names.length === 0 || new Set(names).size !== names.length) throw new Error("planted's test list is empty or has duplicates");
	return names.sort();
}

// Fails unless `index` is a whole number from 0 up to `count`, which is a whole number of at least 1
export function checkShard(index, count) {
	if (!Number.isSafeInteger(count) || count < 1 || !Number.isSafeInteger(index) || index < 0 || index >= count) {
		throw new Error("expected a zero-based shard index and a positive shard count");
	}
}

// What a test without a measured time counts as, in milliseconds
export const RUN_MS = 500;

// The measured seconds of each mode's slow tests (test-weights.json), as whole milliseconds by name
export function readWeights(file = new URL("./test-weights.json", import.meta.url)) {
	const table = JSON.parse(fs.readFileSync(file, "utf8"));
	const weights = {};
	for (const mode of Object.keys(OWN_JOBS)) {
		weights[mode] = new Map();
		for (const [name, seconds] of Object.entries(table[mode] ?? {})) {
			if (typeof seconds !== "number" || !(seconds >= 0)) throw new Error(`test-weights.json: ${mode} ${name} has no time in seconds`);
			weights[mode].set(name, Math.round(seconds * 1000));
		}
	}
	return weights;
}

// The `count` slices of `names`: longest first, each to the slice with the least time so far, the names of each sorted
export function shares(names, weights, count) {
	checkShard(0, count);
	const items = names.map(name => ({ name, ms: weights.get(name) ?? RUN_MS }));
	items.sort((a, b) => b.ms - a.ms || (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
	const loads = new Array(count).fill(0);
	const slices = Array.from({ length: count }, () => []);
	for (const item of items) {
		let least = 0;
		for (let i = 1; i < count; i++) if (loads[i] < loads[least]) least = i;
		slices[least].push(item.name);
		loads[least] += item.ms;
	}
	return slices.map((slice, i) => ({ names: slice.sort(), ms: loads[i] }));
}

// The slice of `names` for shard `index` of `count`
export function partition(names, index, count, weights = new Map()) {
	checkShard(index, count);
	return shares(names, weights, count)[index].names;
}

// The test binaries that run in jobs of their own, by the mode of those jobs: the package each belongs to and its test
// target
export const OWN_JOBS = {
	planted: { packageName: "ree-profile-view", target: "planted" },
	standalone: { packageName: "roblox-engine-emulator", target: "standalone" },
};

// The mode of the jobs that run `binary`: planted or standalone for those two test binaries, core for every other.
// Cargo sets CARGO_PKG_NAME to the package a test binary belongs to, so the name tells packages apart: another
// package's test target named planted must still run in core. A missing name means the package is not known, so core
// runs the binary and the other jobs reject it.
export function jobOf(binary, packageName) {
	const file = path.basename(binary);
	for (const [mode, own] of Object.entries(OWN_JOBS)) {
		if (packageName === own.packageName && new RegExp(`^${own.target}-[0-9a-f]+\\.exe$`).test(file)) return mode;
	}
	return "core";
}

// Splits the command line into the mode, the shard (planted and standalone), the test binary and the arguments for it
function parseArguments(argv) {
	const [mode, ...rest] = argv;
	if (mode !== "core" && !Object.hasOwn(OWN_JOBS, mode)) throw new Error("expected core, planted or standalone runner mode");
	const [index, count, binary, ...testArguments] = mode === "core" ? [null, null, ...rest] : rest;
	if (!binary) throw new Error("missing test binary");
	return { mode, index: Number(index), count: Number(count), binary, testArguments };
}

// Runs the runner and returns the exit code. `cwd`, `env`, `spawn`, `log` and `weights` are replaced by the tests. The
// binary starts in `cwd` with `env`, the folder and variables cargo gave the runner, as if cargo had started it directly.
export function run(argv, { cwd = process.cwd(), env = process.env, spawn = spawnSync, log = console.error, weights } = {}) {
	const { mode, index, count, binary, testArguments } = parseArguments(argv);

	// Starts the test binary; returns its exit code (1 for a signal) and, when `stdio` pipes it, what it printed
	function start(argumentsForBinary, stdio = "inherit") {
		const result = spawn(binary, argumentsForBinary, { cwd, env, windowsHide: true, encoding: "utf8", stdio, maxBuffer: 16 * 1024 * 1024 });
		if (result.error) throw result.error;
		return { code: result.status ?? 1, output: result.stdout };
	}

	const job = jobOf(binary, env.CARGO_PKG_NAME);
	if (mode === "core") {
		if (job !== "core") {
			log(`${job}: its tests run in jobs of their own`);
			return 0;
		}
		return start(testArguments).code;
	}

	if (job !== mode) throw new Error(`the ${mode} job received a different test target`);
	// A bad shard fails here, whatever the binary would list
	checkShard(index, count);
	const listing = start(["--list", "--format", "terse"], ["ignore", "pipe", "inherit"]);
	if (listing.code !== 0) return listing.code;
	const names = parseTestList(listing.output);
	const all = shares(names, (weights ?? readWeights())[mode], count);
	const slice = all[index].names;
	log(`${mode} shard ${index + 1}/${count}: ${slice.length} of ${names.length} tests, about ${Math.round(all[index].ms / 1000)} s (the shards: ${all.map(share => Math.round(share.ms / 1000)).join(" / ")} s)`);
	if (slice.length === 0) return 0;
	// Several test names after the options are run together. --exact makes each name match that test alone, never every
	// test whose name contains it. Planted's one test thread keeps the engine runs one at a time; standalone's tests run
	// on libtest's threads, as they did in one job.
	const threads = mode === "planted" ? ["--test-threads=1"] : [];
	return start(["--exact", ...threads, ...testArguments, ...slice]).code;
}

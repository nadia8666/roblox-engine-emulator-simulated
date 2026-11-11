// The test runner behind the CI jobs (.github/workflows/ci.yml). Cargo starts every test binary through test-entry.mjs,
// which calls run() here:
//
//   node tools/ci/test-entry.mjs core <binary> [test arguments]
//   node tools/ci/test-entry.mjs planted <index> <count> <binary> [test arguments]
//   node tools/ci/test-entry.mjs standalone <binary> [test arguments]
//
// Two test binaries take most of a run's testing time, so each runs in jobs of its own, beside the rest:
//
// - The planted profile tests (profile-view/tests/planted.rs): each runs the engine under the profiler, and one lock
//   around the engine keeps them from running side by side. So CI runs them on `count` machines, each running one slice.
// - The standalone tests (tests/standalone.rs): one test runs the installed binary's checks six times in a row and
//   takes three minutes, longer than every other test of the test job together.
//
// The mode says which job this is:
//
//   core        runs every test binary as cargo asked, except planted's and standalone's, which it skips
//   planted     runs one slice of planted's tests. It lists the binary's tests, sorts the names and takes every
//               `count`-th name, starting at the zero-based `index`. The slices cover the list without overlap, so
//               every test runs in exactly one job, and a new test joins a slice by its name with no list to maintain
//               here.
//   standalone  runs standalone's test binary as cargo asked
//
// The exit code is the test binary's, so a failing test fails the job. A list that is empty, has a duplicate or does
// not read as libtest's fails the job too, and so does a planted or standalone job that is handed another test binary,
// so that no test is lost unseen.
//
// The arguments are positional, unlike the --name value pairs of the release scripts: cargo puts the test binary and
// its arguments after the ones in its runner setting.
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

// The slice of `names` for shard `index` of `count`: the names at positions index, index + count, index + 2 * count...
export function partition(names, index, count) {
	checkShard(index, count);
	return names.filter((_, position) => position % count === index);
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

// Splits the command line into the mode, the shard (planted only), the test binary and the arguments for it
function parseArguments(argv) {
	const [mode, ...rest] = argv;
	if (mode !== "core" && !Object.hasOwn(OWN_JOBS, mode)) throw new Error("expected core, planted or standalone runner mode");
	const [index, count, binary, ...testArguments] = mode === "planted" ? rest : [null, null, ...rest];
	if (!binary) throw new Error("missing test binary");
	return { mode, index: Number(index), count: Number(count), binary, testArguments };
}

// Runs the runner and returns the exit code. `cwd`, `env`, `spawn` and `log` are replaced by the tests. The binary
// starts in `cwd` with `env`, the folder and variables cargo gave the runner, as if cargo had started it directly.
export function run(argv, { cwd = process.cwd(), env = process.env, spawn = spawnSync, log = console.error } = {}) {
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
	if (mode === "standalone") return start(testArguments).code;
	// A bad shard fails here, whatever the binary would list
	checkShard(index, count);
	const listing = start(["--list", "--format", "terse"], ["ignore", "pipe", "inherit"]);
	if (listing.code !== 0) return listing.code;
	const names = parseTestList(listing.output);
	const slice = partition(names, index, count);
	log(`planted shard ${index + 1}/${count}: ${slice.length} of ${names.length} tests`);
	if (slice.length === 0) return 0;
	// Several test names after the options are run together. --exact makes each name match that test alone, never every
	// test whose name contains it, and one test thread keeps the engine runs one at a time.
	return start(["--exact", "--test-threads=1", ...testArguments, ...slice]).code;
}

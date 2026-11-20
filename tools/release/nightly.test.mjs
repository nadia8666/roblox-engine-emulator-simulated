// Tests of nightly.mjs, the nightly run's choice between building main's head and building nothing
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { WORKFLOW } from "./dry-run.mjs";
import { NOT_SHIPPED, decide, globRegex, pushPaths, run, shipsFrom } from "./nightly.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
// This repository's release workflow: the artifacts a release publishes, the jobs that build and test them, and the
// paths whose push starts a dry run
const ARTIFACTS = ["ree-windows-x86_64", "ree-linux-x86_64", "ree-macos-aarch64"];
const BUILD_JOBS = ["build", "smoke"];
const PUSH_PATHS = [".github/workflows/release.yml", "tools/release/**", "**/Cargo.toml", "**/Cargo.lock", "**/build.rs", ".cargo/config.toml", "tools/vendor.mjs", "patches/**", "LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md", "licenses/**"];

// The rest is the same in each tool repository
// A workflow's push paths, as the tests' repositories list them
const LISTED = [".github/workflows/release.yml", "tools/release/**", "**/Cargo.toml", "**/Cargo.lock", ".cargo/config.toml", "LICENSE", "THIRD_PARTY_NOTICES.md", "licenses/**"];
const workflowListing = (paths) => `on:\n  push:\n    branches: [main]\n    paths:\n${paths.map((p) => `      - '${p}'`).join("\n")}\n  schedule:\n    - cron: "0 1 * * *"\n`;
const ARGS = ["--repository", "owner/name", "--artifacts", ARTIFACTS.join(",")];
const runUrl = (id) => `https://github.com/owner/name/actions/runs/${id}`;

const passedJobs = [
	{ name: "version", conclusion: "success" },
	{ name: "build (windows)", conclusion: "success" },
	{ name: "smoke (windows)", conclusion: "success" },
	{ name: "notes", conclusion: "skipped" },
	{ name: "publish", conclusion: "skipped" },
	{ name: "canary", conclusion: "skipped" },
];
// A nightly run that built nothing: only its version job ran
const skippedJobs = passedJobs.map((job) => (job.name === "version" ? job : { ...job, conclusion: "skipped" }));
const allArtifacts = ARTIFACTS.map((name) => ({ name, expired: false }));

function releaseRun(id, sha, fields = {}) {
	return { id, run_number: id, run_attempt: 1, head_sha: sha, head_branch: "main", path: WORKFLOW, event: "push", status: "completed", conclusion: "success", html_url: runUrl(id), ...fields };
}

/** A git repository in a temporary folder, removed after the test; `commit` writes files (null deletes one) and commits. */
function repository(t) {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "release-nightly-test-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	const git = (...argv) => execFileSync("git", argv, { cwd: root, encoding: "utf8" }).trim();
	git("init", "--quiet");
	git("config", "user.name", "Release tests");
	git("config", "user.email", "release-test");
	const commit = (files, message = "A change") => {
		for (const [file, text] of Object.entries(files)) {
			const full = path.join(root, file);
			if (text === null) fs.rmSync(full);
			else {
				fs.mkdirSync(path.dirname(full), { recursive: true });
				fs.writeFileSync(full, text);
			}
		}
		git("add", "--all");
		git("commit", "--quiet", "--allow-empty", "-m", message);
		return git("rev-parse", "HEAD");
	};
	const first = commit({ [WORKFLOW]: workflowListing(LISTED), "src/lib.rs": "fn main() {}\n", "README.md": "# Tool\n" }, "First change");
	return { root, git, commit, first };
}

/** Answers the API from `api.runs` (listed 100 a page), `api.jobs` and `api.artifacts` by run id; gives the URLs asked. */
function mockApi(t, api) {
	const calls = [];
	t.mock.method(globalThis, "fetch", async (url) => {
		calls.push(String(url));
		const id = Number(/\/runs\/(\d+)\//.exec(url)?.[1]);
		let body;
		if (/\/jobs\?/.test(url)) body = { jobs: api.jobs?.[id] ?? passedJobs };
		else if (/\/artifacts\?/.test(url)) body = { artifacts: api.artifacts?.[id] ?? allArtifacts };
		else {
			const page = Number(new URL(url).searchParams.get("page"));
			body = { workflow_runs: (api.runs ?? []).slice((page - 1) * 100, page * 100) };
		}
		return { ok: true, json: async () => body };
	});
	return calls;
}

async function nightly(root, head, logged = []) {
	const output = await run([...ARGS, "--sha", head], { GH_TOKEN: "x" }, (line) => logged.push(line), root);
	const [, build, reason] = /^build=(true|false)\nreason=(.*)$/.exec(output);
	return { build: build === "true", reason };
}

// The path rule

test("nothing under .github/ ships, even the workflow file its push paths list", () => {
	const ships = shipsFrom(LISTED);
	for (const file of [".github/workflows/release.yml", ".github/workflows/ci.yml", ".github/dependabot.yml"]) assert.equal(ships(file), false, file);
});

test("a path the push paths list ships, even a tool, a dot-folder or Markdown", () => {
	const ships = shipsFrom(LISTED);
	for (const file of ["tools/release/package.mjs", "tools/release/a/b.mjs", ".cargo/config.toml", "THIRD_PARTY_NOTICES.md", "LICENSE", "licenses/a/LICENSE.txt", "Cargo.toml", "crates/a/Cargo.toml", "native/Cargo.lock"]) {
		assert.equal(ships(file), true, file);
	}
});

test("any other path with a part that starts with a dot does not ship", () => {
	const ships = shipsFrom(LISTED);
	for (const file of [".cargo/other.toml", ".editorconfig", ".vscode/settings.json", "src/.hidden/a.rs", "crates/a/.keep"]) assert.equal(ships(file), false, file);
});

test("the rest ships unless it is Markdown, docs, notes, tests, benches, examples, fixtures, goldens or a tool", () => {
	const ships = shipsFrom(LISTED);
	for (const file of ["README.md", "crates/a/CHANGELOG.md", "docs/a.png", "notes/plan.txt", "tests/a.rs", "crates/a/tests/b.rs", "benches/b.rs", "crates/a/benches/c.rs", "examples/e.rs", "src/examples/e.rs", "fixtures/shapes.rbxmx", "crates/a/fixtures/b.bin", "goldens/x.png", "crates/a/goldens/x.png", "tools/other.mjs", "tools/a/b.mjs"]) {
		assert.equal(ships(file), false, file);
	}
	for (const file of ["src/lib.rs", "NOTICE", "build.rs", "crates/a/src/tests.rs", "src/testsuite/a.rs", "src/docs/a.rs", "documentation/a.txt", "toolsx/a.mjs", "a.md.rs", "native/src/main.rs"]) {
		assert.equal(ships(file), true, file);
	}
	// with no push paths, a release file is held to the same rules
	assert.equal(shipsFrom([])("tools/release/package.mjs"), false);
	assert.equal(shipsFrom([])("Cargo.lock"), true);
	assert.ok(NOT_SHIPPED.includes("tools/**") && NOT_SHIPPED.includes(".github/**"));
});

test("in a glob, ** is any number of segments, * and ? stay in one, {a,b} is either and a trailing / is everything below", () => {
	const matches = (glob, file) => globRegex(glob).test(file);
	assert.ok(matches("**/tests/**", "tests/a.rs") && matches("**/tests/**", "a/b/tests/c/d.rs"));
	assert.ok(!matches("**/tests/**", "atests/a.rs") && !matches("**/tests/**", "a/tests"));
	assert.ok(matches("**/*.md", "README.md") && matches("**/*.md", "a/b/c.md") && !matches("**/*.md", "a.mdx"));
	assert.ok(matches("tools/*.mjs", "tools/a.mjs") && !matches("tools/*.mjs", "tools/a/b.mjs"));
	assert.ok(matches("tools/release/**", "tools/release/a/b.mjs") && !matches("tools/release/**", "tools/releases/a.mjs"));
	assert.ok(matches("src/?.rs", "src/a.rs") && !matches("src/?.rs", "src/ab.rs") && !matches("src/?.rs", "src//.rs"));
	assert.ok(matches("{LICENSE,NOTICE}", "LICENSE") && matches("{LICENSE,NOTICE}", "NOTICE") && !matches("{LICENSE,NOTICE}", "NOTE"));
	assert.ok(matches("licenses/", "licenses/a/b.txt") && !matches("licenses/", "licenses"));
	assert.ok(!matches("a.b", "axb") && matches("a+b(c)", "a+b(c)"));
	assert.throws(() => globRegex("{a,b"), /unclosed/);
});

test("the push paths are the block list under paths:, plain or quoted, past comments, up to the next key", () => {
	const text = [
		"on:",
		"  push:",
		"    branches: [main]",
		"    # comment",
		"    tags: ['v[0-9]+.[0-9]+.[0-9]+', canary]",
		"    paths:",
		"      - .github/workflows/release.yml",
		"      # a comment between items",
		"      - 'tools/release/**'",
		"",
		'      - "**/Cargo.lock" # with a comment',
		"  schedule:",
		'    - cron: "0 1 * * *"',
	].join("\r\n");
	assert.deepEqual(pushPaths(text), [".github/workflows/release.yml", "tools/release/**", "**/Cargo.lock"]);
	assert.deepEqual(pushPaths("on:\n  workflow_dispatch:\n"), []);
});

// The decision

test("the newest commit with a passing run is the base, and the head as base needs no diff", async () => {
	const runs = { c: { html_url: "c-run" }, a: { html_url: "a-run" } };
	const diffs = [];
	const decision = (head, changed = []) => decide({ head, history: [head, "c", "b", "a"].filter((sha, i, all) => all.indexOf(sha) === i), passingRun: async (sha) => runs[sha] ?? null, changed: (base, to) => (diffs.push([base, to]), changed), ships: shipsFrom(LISTED) });
	assert.deepEqual(await decision("c"), { build: false, reason: "head already built by run c-run", base: "c", run: runs.c, shipping: [] });
	assert.deepEqual(diffs, []);
	const many = Array.from({ length: 12 }, (_, i) => `src/m${i}.rs`);
	const built = await decision("d", ["README.md", ...many]);
	assert.equal(built.build, true);
	assert.equal(built.base, "c");
	assert.deepEqual(diffs, [["c", "d"]]);
	assert.equal(built.reason, `builds every platform at d: 12 changed path(s) that ship since c, built by run c-run: ${many.slice(0, 10).join(", ")}, and 2 more`);
	const none = await decide({ head: "d", history: ["d", "c"], passingRun: async () => null, changed: () => assert.fail("no diff without a base"), ships: () => true });
	assert.deepEqual(none, { build: true, reason: "builds every platform at d: no commit of its first-parent history has a passing run that kept every zip", base: null, run: null, shipping: [] });
});

test("a nightly run builds nothing when only docs, tests, tools and CI changed since the newest passing run", async (t) => {
	const { root, commit, first } = repository(t);
	const head = commit({ "README.md": "# Tool, documented\n", "tests/a.rs": "#[test] fn a() {}\n", "tools/x.mjs": "", ".github/workflows/ci.yml": "on: push\n", "docs/a.md": "" });
	const calls = mockApi(t, { runs: [releaseRun(1, first)] });
	assert.deepEqual(await nightly(root, head), { build: false, reason: `nothing that ships changed since ${first}, built by run ${runUrl(1)}` });
	assert.match(calls[0], /\/repos\/owner\/name\/actions\/workflows\/release\.yml\/runs\?branch=main&status=success&/);
});

test("a nightly run builds when a path that ships changed since the newest passing run, and names it", async (t) => {
	const { root, commit, first } = repository(t);
	commit({ "README.md": "# Tool, documented\n" });
	const head = commit({ "src/lib.rs": "fn main() { run() }\n", "tests/b.rs": "" });
	mockApi(t, { runs: [releaseRun(1, first)] });
	assert.deepEqual(await nightly(root, head), { build: true, reason: `builds every platform at ${head}: 1 changed path(s) that ship since ${first}, built by run ${runUrl(1)}: src/lib.rs` });
});

test("a head with a passing run of its own builds nothing", async (t) => {
	const { root, commit, first } = repository(t);
	const head = commit({ "src/lib.rs": "fn main() { run() }\n" });
	mockApi(t, { runs: [releaseRun(2, head, { event: "schedule" }), releaseRun(1, first)] });
	assert.deepEqual(await nightly(root, head), { build: false, reason: `head already built by run ${runUrl(2)}` });
});

test("the newest base counts, and a nightly run that built nothing or a run that lost a zip is passed over", async (t) => {
	const { root, commit, first } = repository(t);
	const built = commit({ "src/lib.rs": "fn main() { run() }\n" });
	const docs = commit({ "README.md": "# Tool, documented\n" });
	const head = commit({ "docs/guide.md": "" });
	const expired = allArtifacts.map((a, i) => (i === 0 ? { ...a, expired: true } : a));
	mockApi(t, {
		runs: [releaseRun(4, docs, { event: "schedule" }), releaseRun(3, docs), releaseRun(2, built), releaseRun(1, first)],
		jobs: { 4: skippedJobs },
		artifacts: { 3: expired, 4: [] },
	});
	const logged = [];
	// src/lib.rs changed since the first commit, but not since the newer one a run built
	assert.deepEqual(await nightly(root, head, logged), { build: false, reason: `nothing that ships changed since ${built}, built by run ${runUrl(2)}` });
	assert.match(logged.join("\n"), /passed over run 4 .*build \(windows\) skipped/);
	assert.match(logged.join("\n"), new RegExp(`passed over run 3 .*no artifact ${ARTIFACTS[0]}`));
});

test("a nightly run builds when no commit of the first-parent history has a passing run", async (t) => {
	const { root, git, commit, first } = repository(t);
	git("checkout", "--quiet", "-b", "side");
	const side = commit({ "src/side.rs": "" });
	git("checkout", "--quiet", "-");
	commit({ "README.md": "# Tool, documented\n" });
	git("merge", "--quiet", "--no-ff", "-m", "Merge side", "side");
	const head = git("rev-parse", "HEAD");
	// the side branch's commit had a passing run, and the first commit's failed or ran another workflow
	mockApi(t, { runs: [releaseRun(3, side), releaseRun(2, first, { conclusion: "failure" }), releaseRun(1, first, { path: ".github/workflows/ci.yml" })] });
	assert.deepEqual(await nightly(root, head), { build: true, reason: `builds every platform at ${head}: no commit of its first-parent history has a passing run that kept every zip` });
});

test("a file renamed out of the shipped paths counts as its old path too", async (t) => {
	const { root, git, first } = repository(t);
	fs.mkdirSync(path.join(root, "docs"));
	git("mv", "src/lib.rs", "docs/lib.md");
	git("commit", "--quiet", "-m", "Move");
	const head = git("rev-parse", "HEAD");
	mockApi(t, { runs: [releaseRun(1, first)] });
	assert.deepEqual(await nightly(root, head), { build: true, reason: `builds every platform at ${head}: 1 changed path(s) that ship since ${first}, built by run ${runUrl(1)}: src/lib.rs` });
});

test("the push paths come from the head's workflow, whose own change never ships", async (t) => {
	const { root, commit, first } = repository(t);
	const workflowOnly = commit({ [WORKFLOW]: workflowListing([...LISTED, "docs/**"]) });
	const head = commit({ "docs/a.md": "", "tools/release/x.mjs": "" });
	mockApi(t, { runs: [releaseRun(1, first)] });
	assert.deepEqual(await nightly(root, workflowOnly), { build: false, reason: `nothing that ships changed since ${first}, built by run ${runUrl(1)}` });
	assert.equal((await nightly(root, head)).reason, `builds every platform at ${head}: 2 changed path(s) that ship since ${first}, built by run ${runUrl(1)}: docs/a.md, tools/release/x.mjs`);
});

test("runs past the first page are read, up to a limit", async (t) => {
	const { root, commit, first } = repository(t);
	const head = commit({ "README.md": "# Tool, documented\n" });
	const others = Array.from({ length: 100 }, (_, i) => releaseRun(1000 + i, "f".repeat(40)));
	const api = { runs: [...others, releaseRun(1, first)] };
	const calls = mockApi(t, api);
	assert.equal((await nightly(root, head)).build, false);
	assert.equal(calls.filter((url) => /\/workflows\/release\.yml\/runs\?/.test(url)).length, 2);
	// with every page full of other commits' runs, the listing stops and the run builds
	api.runs = Array.from({ length: 1000 }, (_, i) => releaseRun(1000 + i, "f".repeat(40)));
	calls.length = 0;
	assert.equal((await nightly(root, head)).build, true);
	assert.ok(calls.length <= 5, calls.length);
});

test("the command line takes each option once, and anything that keeps it from deciding builds", (t) => {
	const { root, first } = repository(t);
	const script = path.join(ROOT, "tools/release/nightly.mjs");
	const start = (argv, env = {}) => spawnSync(process.execPath, [script, ...argv], { cwd: root, env: { ...process.env, GH_TOKEN: "x", GITHUB_TOKEN: "", ...env }, encoding: "utf8" });
	for (const argv of [[...ARGS, "--sha", first, "--sha", first], [...ARGS, "--sha", first, "--required"], [...ARGS, "--sha", "abc"], ["--sha", first]]) {
		const bad = start(argv);
		assert.equal(bad.status, 1, argv.join(" "));
		assert.equal(bad.stdout, "");
	}
	// nothing listens on port 1, so the first request fails at once
	const offline = start([...ARGS, "--sha", first], { GITHUB_API_URL: "http://127.0.0.1:1" });
	assert.equal(offline.status, 0, offline.stderr);
	assert.match(offline.stdout, /^build=true\nreason=builds every platform: could not tell whether anything that ships changed since a passing run: [^\n]+\n$/);
	assert.match(offline.stderr, /::warning::/);
	// a commit the checkout doesn't have builds too
	const unknown = start([...ARGS, "--sha", "e".repeat(40)], { GITHUB_API_URL: "http://127.0.0.1:1" });
	assert.equal(unknown.status, 0, unknown.stderr);
	assert.match(unknown.stdout, /^build=true\n/);
});

// This repository's workflow

const workflow = () => fs.readFileSync(path.join(ROOT, WORKFLOW), "utf8").replace(/\r\n/g, "\n"); // Windows checkouts may have CRLF line ends
/** The text of one job, from its key to the next job's. */
function job(text, name) {
	const start = text.indexOf(`\n  ${name}:\n`);
	assert.ok(start >= 0, `no job ${name}`);
	const next = text.slice(start + 1).search(/\n  [\w-]+:\n/);
	return next < 0 ? text.slice(start) : text.slice(start, start + 1 + next);
}

test("this workflow's push paths are read in full", () => {
	assert.deepEqual(pushPaths(workflow()), PUSH_PATHS);
});

test("a nightly run asks nightly.mjs, says why in its summary, and builds only when it says so", () => {
	const text = workflow();
	const version = job(text, "version");
	assert.match(version, /\n      nightly-build: \$\{\{ steps\.nightly\.outputs\.build \}\}\n/);
	const decision = /- name: Decide whether the nightly run builds\n\s+id: nightly\n\s+if: \$\{\{ github\.event_name == 'schedule' \}\}\n[^]*?\n\s+run: node tools\/release\/nightly\.mjs --repository "\$GITHUB_REPOSITORY" --sha "\$GITHUB_SHA" --artifacts (\S+) >> "\$GITHUB_OUTPUT"\n/.exec(version);
	assert.ok(decision, "the decision step");
	assert.deepEqual(decision[1].split(","), ARTIFACTS);
	assert.match(decision[0], /GH_TOKEN: \$\{\{ github\.token \}\}/);
	assert.match(version, /- name: Say what the nightly run builds and why\n\s+if: \$\{\{ github\.event_name == 'schedule' \}\}\n[^]*?REASON: \$\{\{ steps\.nightly\.outputs\.reason \}\}\n\s+run: printf '%s\\n' "\$REASON" \| tee -a "\$GITHUB_STEP_SUMMARY"\n/);
	// only a tag looks for a passing dry run of its own commit now
	assert.match(text, /name: Find a passing dry run of this commit\n\s+id: dry-run\n\s+if: \$\{\{ github\.event_name == 'push' && github\.ref_type == 'tag' \}\}\n/);
	for (const name of BUILD_JOBS) {
		assert.match(job(text, name), /\n    if: \$\{\{ !github\.event\.repository\.private && needs\.version\.outputs\.dry-run == '' && needs\.version\.outputs\.nightly-build != 'false' \}\}\n/, name);
	}
});

test("a nightly run that builds nothing uploads nothing: only the build jobs and the tag-only jobs upload", () => {
	const text = workflow();
	const jobs = [...text.split(/^jobs:\n/m)[1].matchAll(/^  ([\w-]+):\n/gm)].map((m) => m[1]);
	for (const name of jobs) {
		if (!job(text, name).includes("actions/upload-artifact@")) continue;
		assert.ok(BUILD_JOBS.includes(name) || /\n    if: \$\{\{[^\n]*github\.event_name == 'push' && github\.ref_type == 'tag'/.test(job(text, name)), `${name} uploads on a nightly run`);
	}
});

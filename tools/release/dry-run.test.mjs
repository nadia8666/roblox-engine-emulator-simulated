// Tests of dry-run.mjs, the choice of the dry run whose zips a tag publishes
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { WORKFLOW, pickDryRun, run } from "./dry-run.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
// The artifacts a release publishes: one zip per platform
const ARTIFACTS = ["ree-windows-x86_64", "ree-linux-x86_64", "ree-macos-aarch64"];
const SHA = "a".repeat(40);

function dryRun(id, fields = {}) {
	return { id, run_number: id, run_attempt: 1, head_sha: SHA, head_branch: "main", path: WORKFLOW, event: "push", status: "completed", conclusion: "success", ...fields };
}
const passedJobs = [
	{ name: "version", conclusion: "success" },
	{ name: "build (windows)", conclusion: "success" },
	{ name: "smoke (windows)", conclusion: "success" },
	{ name: "notes", conclusion: "skipped" },
	{ name: "publish", conclusion: "skipped" },
	{ name: "canary", conclusion: "skipped" },
];
const allArtifacts = ARTIFACTS.map((name) => ({ name, expired: false }));

function pick(runs, { jobs = {}, artifacts = {} } = {}) {
	return pickDryRun({
		runs,
		jobsOf: async (r) => jobs[r.id] ?? passedJobs,
		artifactsOf: async (r) => artifacts[r.id] ?? allArtifacts,
		sha: SHA,
		branch: "main",
		artifacts: ARTIFACTS,
	});
}

// Answers the API's three requests from `runs`, with every job passed and every artifact kept
function mockApi(t, runs) {
	const calls = [];
	t.mock.method(globalThis, "fetch", async (url) => {
		calls.push(String(url));
		const body = /\/jobs\?/.test(url) ? { jobs: passedJobs } : /\/artifacts\?/.test(url) ? { artifacts: allArtifacts } : { workflow_runs: runs };
		return { ok: true, json: async () => body };
	});
	return calls;
}
const ARGS = ["--repository", "owner/name", "--sha", SHA, "--artifacts", ARTIFACTS.join(",")];

test("a tag publishes the newest dry run of its commit that passed every job and kept every zip", async () => {
	const { run: found } = await pick([dryRun(10), dryRun(12, { event: "workflow_dispatch" }), dryRun(11)]);
	assert.equal(found.id, 12);
});

test("a nightly run that built and tested qualifies like a push", async () => {
	const { run: found } = await pick([dryRun(10), dryRun(11, { event: "schedule" })]);
	assert.equal(found.id, 11);
});

test("runs of another commit, branch, workflow file or event are never taken", async () => {
	const { run: found, reasons } = await pick([
		dryRun(1, { head_sha: "b".repeat(40) }),
		dryRun(2, { head_branch: "other" }),
		dryRun(3, { path: ".github/workflows/ci.yml" }),
		dryRun(4, { event: "pull_request" }),
		dryRun(5, { event: "workflow_run" }),
	]);
	assert.equal(found, null);
	assert.equal(reasons.length, 2, "the two runs of the right file, branch and commit are passed over by event");
});

test("a dry run that failed, is still going, or lost a zip is passed over for an older one that passed", async () => {
	const runs = [dryRun(1), dryRun(2, { conclusion: "failure" }), dryRun(3, { status: "in_progress", conclusion: null }), dryRun(4), dryRun(5), dryRun(6)];
	const jobs = {
		4: [...passedJobs, { name: "smoke (macos)", conclusion: "cancelled" }],
		5: [...passedJobs.slice(0, 3), { name: "notes", conclusion: "skipped" }, { name: "build (linux)", conclusion: "skipped" }],
	};
	const artifacts = { 6: allArtifacts.map((a, i) => (i === 2 ? { ...a, expired: true } : a)) };
	const { run: found, reasons } = await pick(runs, { jobs, artifacts });
	assert.equal(found.id, 1);
	assert.equal(reasons.length, 5);
	assert.match(reasons.join("\n"), new RegExp(`run 6 .*no artifact ${ARTIFACTS[2]}`));
	assert.match(reasons.join("\n"), /run 5 .*build \(linux\) skipped/);
});

test("a nightly run that skipped its builds, because the commit had passed already, is passed over", async () => {
	const skippedBuilds = [{ name: "version", conclusion: "success" }, ...passedJobs.slice(1, 3).map((job) => ({ ...job, conclusion: "skipped" })), ...passedJobs.slice(3)];
	const { run: found, reasons } = await pick([dryRun(1), dryRun(2, { event: "schedule" })], { jobs: { 2: skippedBuilds }, artifacts: { 2: [] } });
	assert.equal(found.id, 1);
	assert.match(reasons[0], /run 2 .*jobs not passed: build \(windows\) skipped/);
});

test("no run found leaves the run id and date empty, so the release builds", async (t) => {
	const calls = mockApi(t, []);
	const line = await run(ARGS, { GH_TOKEN: "x" }, () => {});
	assert.equal(line, "run-id=\nrun-date=");
	assert.match(calls[0], new RegExp(`/repos/owner/name/actions/workflows/release.yml/runs\\?head_sha=${SHA}&branch=main`));
});

test("a run found gives its id and the time it started", async (t) => {
	mockApi(t, [dryRun(7, { run_started_at: "2026-11-15T01:51:00Z", created_at: "2026-11-15T01:50:00Z" })]);
	assert.equal(await run(ARGS, { GH_TOKEN: "x" }, () => {}), "run-id=7\nrun-date=2026-11-15T01:51:00Z");
	assert.equal(await run([...ARGS, "--required"], { GH_TOKEN: "x" }, () => {}), "run-id=7\nrun-date=2026-11-15T01:51:00Z");
});

test("--required fails when no run qualifies instead of leaving the run id empty", async (t) => {
	mockApi(t, [dryRun(7, { conclusion: "failure" })]);
	const logged = [];
	await assert.rejects(run(["--required", ...ARGS], { GH_TOKEN: "x" }, (line) => logged.push(line)), /nothing to publish, and this run never builds/);
	assert.match(logged.join("\n"), /passed over run 7 .*completed failure/);
});

test("the command line takes each option once and --required without a value", async () => {
	await assert.rejects(run([...ARGS, "--required", "--required"], { GH_TOKEN: "x" }), /usage/);
	await assert.rejects(run([...ARGS, "--sha", SHA], { GH_TOKEN: "x" }), /usage/);
	await assert.rejects(run([...ARGS, "--other", "x"], { GH_TOKEN: "x" }), /usage/);
	await assert.rejects(run(["--repository", "owner/name", "--sha", "abc", "--artifacts", "x"], { GH_TOKEN: "x" }), /full --sha/);
});

test("an API failure builds without --required and fails the step with it", () => {
	const script = path.join(ROOT, "tools/release/dry-run.mjs");
	// Nothing listens on port 1, so the first request fails at once
	const env = { ...process.env, GH_TOKEN: "x", GITHUB_TOKEN: "", GITHUB_API_URL: "http://127.0.0.1:1" };
	const plain = spawnSync(process.execPath, [script, ...ARGS], { env, encoding: "utf8" });
	assert.equal(plain.status, 0, plain.stderr);
	assert.equal(plain.stdout, "run-id=\nrun-date=\n");
	assert.match(plain.stderr, /::warning::/);
	const required = spawnSync(process.execPath, [script, ...ARGS, "--required"], { env, encoding: "utf8" });
	assert.equal(required.status, 1);
	assert.equal(required.stdout, "");
	assert.match(required.stderr, /::error::/);
});

// The rest checks this repository's workflow
const workflow = () => fs.readFileSync(path.join(ROOT, WORKFLOW), "utf8").replace(/\r\n/g, "\n"); // Windows checkouts may have CRLF line ends

test("the workflow asks for every platform's zip and publishes from the dry run only on a tag", () => {
	const text = workflow();
	assert.deepEqual(/--artifacts (\S+)/.exec(text)[1].split(","), ARTIFACTS);
	// a tag looks for a passing run of the commit (a nightly run asks nightly.mjs); only the canary tag requires one
	assert.match(text, /name: Find a passing dry run of this commit\n\s+id: dry-run\n\s+if: \$\{\{ github.event_name == 'push' && github.ref_type == 'tag' \}\}\n/);
	assert.match(text, /REQUIRED: \$\{\{ github.event_name == 'push' && github.ref == 'refs\/tags\/canary' && '--required' \|\| '' \}\}\n/);
	assert.match(text, /--artifacts \S+ \$REQUIRED >> "\$GITHUB_OUTPUT"/);
	// building and smoke-testing are skipped only when a passing run was found, or a nightly run has nothing to build
	for (const job of ["build", "smoke"]) {
		assert.match(text, new RegExp(`\\n  ${job}:\\n    if: \\$\\{\\{ !github.event.repository.private && needs.version.outputs.dry-run == '' && needs.version.outputs.nightly-build != 'false' \\}\\}`));
	}
	// and the downloads from another run are the publishing jobs' alone (publish, then canary)
	assert.equal(text.match(/run-id: \$\{\{ needs.version.outputs.dry-run \}\}/g).length, text.split(/^  publish:/m)[1].match(/run-id: /g).length);
});

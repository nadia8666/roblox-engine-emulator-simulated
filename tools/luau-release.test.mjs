// Tests of the Luau release tracker (tools/luau-release.mjs) and of what its workflow must keep:
// node --test tools/luau-release.test.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import { test } from "node:test";
import { clientRelease, decide, parseTags, parseTry, plan, renderIssue } from "./luau-release.mjs";

const WORKFLOW = fs.readFileSync(new URL("../.github/workflows/luau-upstream.yml", import.meta.url), "utf8").replace(/\r\n/g, "\n");
const BOT = { login: "github-actions[bot]" };

const sha = digit => digit.repeat(40);
const TAGS = [
	`${sha("a")}\trefs/tags/0.99`,
	`${sha("b")}\trefs/tags/0.746`,
	`${sha("c")}\trefs/tags/0.746^{}`,
	`${sha("d")}\trefs/tags/0.100`,
	`${sha("e")}\trefs/tags/v1.0`,
	`${sha("f")}\trefs/tags/0.747-rc`,
	"",
].join("\n");

// What `node tools/vendor.mjs try` prints when the series applies, and when two topics conflict
const CLEAN = "Luau 0.746 -> 0.747 (abc):\n\nThe series applies to Luau 0.747 as it is, with 1 backports dropped: `node tools/vendor.mjs bump client` needs no hand merge.\n";
const CONFLICTED = [
	"Luau 0.746 -> 0.747 (abc):",
	"  03-profiling: CONFLICTS in VM/src/lvmexecute.cpp (lines 120, 480), VM/src/lgc.cpp (line 12)",
	"  09-hooks: CONFLICTS in VM/src/ldo.cpp (line 77)",
	"",
	"2 of 14 topics conflict with Luau 0.747 (later topics were tried without them); `bump` stops at each for a hand merge.",
	"",
].join("\n");

test("release tags are read from ls-remote in release order, each once", () => {
	assert.deepEqual(parseTags(TAGS), ["0.99", "0.100", "0.746"]);
	assert.deepEqual(parseTags(TAGS.replaceAll("\n", "\r\n")), ["0.99", "0.100", "0.746"]);
});

test("a client version names the release it runs", () => {
	assert.equal(clientRelease("0.746.0.7461267"), "0.746");
	assert.equal(clientRelease("version-abc"), null);
});

test("a pin that is the newest release and the client's is behind nothing", () => {
	assert.equal(plan({ pinned: "0.746", tags: ["0.745", "0.746"], client: "0.746" }), null);
	assert.equal(plan({ pinned: "0.746", tags: ["0.745", "0.746"], client: null }), null);
	assert.equal(plan({ pinned: "0.746", tags: ["0.745"], client: "0.745" }), null);
});

test("the client's release is the target when a client runs one newer than the pin", () => {
	const tagged = plan({ pinned: "0.746", tags: ["0.746", "0.747", "0.748"], client: "0.747" });
	assert.deepEqual(tagged, { pinned: "0.746", target: "0.747", spec: "client", newestTag: "0.748", client: "0.747", tagged: true });
	const untagged = plan({ pinned: "0.746", tags: ["0.746"], client: "0.747", synced: true });
	assert.equal(untagged.target, "0.747");
	assert.equal(untagged.spec, "client");
	assert.equal(untagged.tagged, false);
});

test("a release a client runs before upstream has published it is nothing to build yet", () => {
	assert.equal(plan({ pinned: "0.746", tags: ["0.746"], client: "0.747" }), null);
	// An older tag the pin is behind is still the target meanwhile
	const behind = plan({ pinned: "0.745", tags: ["0.745", "0.746"], client: "0.747" });
	assert.equal(behind.target, "0.746");
	assert.equal(behind.spec, "0.746");
	const issue = renderIssue(behind, parseTry({ status: 0, stdout: CLEAN, stderr: "" }));
	assert.match(issue.body, /already runs 0\.747, which upstream has not published yet: 0\.746 is the newest release there is to build/);
});

test("a tag no client runs yet is the target by its own name", () => {
	const behind = plan({ pinned: "0.746", tags: ["0.746", "0.747"], client: "0.746" });
	assert.equal(behind.target, "0.747");
	assert.equal(behind.spec, "0.747");
	assert.equal(plan({ pinned: "0.746", tags: ["0.746", "0.747"], client: null }).spec, "0.747");
});

test("the outcome of `try` is read from its output and its exit status", () => {
	assert.deepEqual(parseTry({ status: 0, stdout: CLEAN, stderr: "" }), { tried: true, applies: true, conflicts: [], error: null });
	const conflicted = parseTry({ status: 1, stdout: CONFLICTED, stderr: "" });
	assert.equal(conflicted.applies, false);
	assert.deepEqual(conflicted.conflicts, [
		{ topic: "03-profiling", where: "VM/src/lvmexecute.cpp (lines 120, 480), VM/src/lgc.cpp (line 12)" },
		{ topic: "09-hooks", where: "VM/src/ldo.cpp (line 77)" },
	]);
	const failed = parseTry({ status: 2, stdout: "", stderr: "vendor: target\nvendor: https://example.invalid: 404\n" });
	assert.equal(failed.tried, false);
	assert.match(failed.error, /404/);
	assert.equal(parseTry({ status: null, stdout: "", stderr: "killed" }).tried, false);
});

test("the issue names the release, the client and each conflicting topic", () => {
	const behind = plan({ pinned: "0.746", tags: ["0.746", "0.747", "0.748"], client: "0.747" });
	const issue = renderIssue(behind, parseTry({ status: 1, stdout: CONFLICTED, stderr: "" }));
	assert.equal(issue.title, "Update Luau to 0.747");
	assert.match(issue.body, /releases\/tag\/0\.747/);
	assert.match(issue.body, /compare\/0\.746\.\.\.0\.747/);
	assert.match(issue.body, /A live Roblox client runs 0\.747/);
	assert.match(issue.body, /Upstream has also tagged 0\.748/);
	assert.match(issue.body, /2 topics of the series conflict/);
	assert.match(issue.body, /- `03-profiling`: VM\/src\/lvmexecute\.cpp \(lines 120, 480\)/);
	assert.match(issue.body, /vendor\.mjs bump client/);
	assert.equal(renderIssue(behind, parseTry({ status: 1, stdout: CONFLICTED, stderr: "" })).body, issue.body);
});

test("the issue says so when the series applies, when no client runs the release, and when the try failed", () => {
	const ahead = plan({ pinned: "0.746", tags: ["0.746", "0.747"], client: "0.746" });
	const clean = renderIssue(ahead, parseTry({ status: 0, stdout: CLEAN, stderr: "" }));
	assert.match(clean.body, /applies to 0\.747 as it is: `node tools\/vendor\.mjs bump 0\.747`/);
	assert.match(clean.body, /still runs 0\.746, the release the engine builds/);
	const untagged = renderIssue(plan({ pinned: "0.746", tags: ["0.746"], client: "0.747", synced: true }), parseTry({ status: 2, stdout: "", stderr: "vendor: the archive's hash differs" }));
	assert.match(untagged.body, /has not tagged it yet/);
	assert.match(untagged.body, /could not try the series/);
	assert.match(untagged.body, /the archive's hash differs/);
	assert.doesNotMatch(untagged.body, /releases\/tag/);
	const unknown = renderIssue(plan({ pinned: "0.746", tags: ["0.746", "0.747"], client: null }), parseTry({ status: 0, stdout: CLEAN, stderr: "" }));
	assert.match(unknown.body, /could not be read this time/);
});

const WANTED = { title: "Update Luau to 0.747", body: "body" };

test("a release with no tracking issue opens one", () => {
	assert.deepEqual(decide([], WANTED), [{ type: "open", ...WANTED }]);
	// Someone else's issue with the same title, and a pull request, are not the tool's
	const others = [
		{ number: 3, title: WANTED.title, body: "body", user: { login: "someone" } },
		{ number: 4, title: WANTED.title, body: "body", user: BOT, pull_request: {} },
	];
	assert.deepEqual(decide(others, WANTED), [{ type: "open", ...WANTED }]);
});

test("the same release with the same facts changes nothing, and new facts rewrite the body without a comment", () => {
	const issue = { number: 7, title: WANTED.title, body: "body", user: BOT };
	assert.deepEqual(decide([issue], WANTED), []);
	assert.deepEqual(decide([{ ...issue, body: "bo\r\ndy" }], { ...WANTED, body: "bo\ndy" }), []);
	assert.deepEqual(decide([{ ...issue, body: "old" }], WANTED), [{ type: "update", number: 7, ...WANTED, comment: null }]);
});

test("a newer release moves the open issue and says so", () => {
	const issue = { number: 7, title: "Update Luau to 0.746", body: "old", user: BOT };
	assert.deepEqual(decide([issue], WANTED), [{ type: "update", number: 7, ...WANTED, comment: "Luau 0.747 is out; this issue now tracks it." }]);
});

test("the issue closes once the pin has caught up, and a second issue closes in favour of the first", () => {
	const first = { number: 7, title: WANTED.title, body: "body", user: BOT };
	const second = { number: 9, title: "Update Luau to 0.746", body: "body", user: BOT };
	assert.deepEqual(decide([first], null), [{ type: "close", number: 7, comment: "The pinned Luau has reached this release." }]);
	assert.deepEqual(decide([second, first], WANTED), [{ type: "close", number: 9, comment: "#7 tracks the Luau update." }]);
	assert.deepEqual(decide([], null), []);
});

test("the workflow runs the tracker on the public repository with the right to write issues, and nothing else", () => {
	assert.match(WORKFLOW, /run: node tools\/luau-release\.mjs\n/);
	assert.match(WORKFLOW, /if: \$\{\{ !github\.event\.repository\.private \}\}/);
	assert.match(WORKFLOW, /permissions:\n  contents: read\n  issues: write\n/);
	assert.match(WORKFLOW, /persist-credentials: false/);
	assert.match(WORKFLOW, /schedule:\n    - cron: /);
});

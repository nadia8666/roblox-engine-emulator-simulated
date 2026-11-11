// Tracks upstream Luau releases as an issue of this repository (docs/building.md "Updating Luau"). When upstream has
// published a release newer than the pinned one (a tag of luau-lang/luau, or the release a live Roblox client runs once
// upstream's master has it), it opens "Update Luau to 0.NNN" with what `node tools/vendor.mjs try` says about the patch
// series and the steps of the update. A later release moves the same issue, and the issue is closed once the pin has
// caught up.
//
//   node tools/luau-release.mjs             open, move or close the issue (GITHUB_TOKEN and GITHUB_REPOSITORY, as a
//                                           workflow sets them)
//   node tools/luau-release.mjs --dry-run   print what it would do; changes nothing
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PIN = path.join(ROOT, "patches", "luau", "upstream.json");
const UPSTREAM = "https://github.com/luau-lang/luau";
const CLIENT_VERSION = "https://clientsettingscdn.roblox.com/v2/client-version/WindowsPlayer";
const USER_AGENT = "ree-luau-release (+https://github.com/Anime-Vanguards/roblox-engine-emulator)";
// The account a workflow's own token writes as: only its issues are this tool's to move or close
const AUTHOR = "github-actions[bot]";
const TITLE = /^Update Luau to (0\.\d+)$/;

export function releaseNumber(release) {
	return Number(release.slice(2));
}

/** The release tags (0.NNN) in `git ls-remote --tags` output. */
export function parseTags(text) {
	const tags = new Set();
	for (const line of text.split("\n")) {
		const match = /^[0-9a-f]{40}\trefs\/tags\/(0\.\d+)(?:\^\{\})?$/.exec(line.trim());
		if (match) tags.add(match[1]);
	}
	return [...tags].sort((a, b) => releaseNumber(a) - releaseNumber(b));
}

/** The Luau release a client version runs: its second field (0.746.0.7461267 runs 0.746). */
export function clientRelease(version) {
	const match = /^0\.(\d+)\./.exec(version);
	return match ? `0.${match[1]}` : null;
}

/**
 * What the pin is behind, or null when there is nothing newer to build. `target` is the release the issue is about: the
 * client's when a client runs one newer than the pin and upstream has published it (the engine follows the client), else
 * the newest tag. `synced` says upstream's master has the client's release before its tag exists. `spec` is the
 * tools/vendor.mjs target that reaches the release.
 */
export function plan({ pinned, tags, client, synced = false }) {
	const newestTag = tags.length ? tags[tags.length - 1] : null;
	const clientAhead = client !== null && releaseNumber(client) > releaseNumber(pinned) && (tags.includes(client) || synced);
	const tagAhead = newestTag !== null && releaseNumber(newestTag) > releaseNumber(pinned);
	if (!clientAhead && !tagAhead) return null;
	const target = clientAhead ? client : newestTag;
	return { pinned, target, spec: clientAhead ? "client" : target, newestTag, client, tagged: tags.includes(target) };
}

/** What `node tools/vendor.mjs try` printed: the topics that conflict, and whether the series applies. */
export function parseTry({ status, stdout, stderr }) {
	if (status !== 0 && status !== 1) {
		return { tried: false, applies: false, conflicts: [], error: (stderr || stdout).trim().split("\n").slice(-3).join("\n") };
	}
	const conflicts = [];
	for (const line of stdout.split("\n")) {
		const match = /^\s+(\S+): CONFLICTS in (.+)$/.exec(line.trimEnd());
		if (match) conflicts.push({ topic: match[1], where: match[2] });
	}
	return { tried: true, applies: status === 0 && conflicts.length === 0, conflicts, error: null };
}

function seriesLines({ target, spec }, attempt) {
	if (!attempt.tried) {
		return [
			`\`node tools/vendor.mjs try ${spec}\` could not try the series:`,
			"",
			"```",
			attempt.error,
			"```",
		];
	}
	if (attempt.applies) return [`The series applies to ${target} as it is: \`node tools/vendor.mjs bump ${spec}\` needs no hand merge.`];
	return [
		`${attempt.conflicts.length} topic${attempt.conflicts.length === 1 ? "" : "s"} of the series conflict with ${target} (later topics were tried without them). \`bump\` stops at each for a hand merge:`,
		"",
		...attempt.conflicts.map(({ topic, where }) => `- \`${topic}\`: ${where}`),
	];
}

function clientLine({ pinned, target, newestTag, client, tagged }) {
	if (client === null) return "Which release a live Roblox client runs could not be read this time.";
	if (client === target) {
		const upstream = newestTag !== null && releaseNumber(newestTag) > releaseNumber(target) ? ` Upstream has also tagged ${newestTag}, which no client runs yet.` : "";
		const untagged = tagged ? "" : ` Upstream has not tagged it yet: \`bump client\` takes the commit that synced it.`;
		return `A live Roblox client runs ${client}, so scripts there already see what this release changed.${untagged}${upstream}`;
	}
	if (releaseNumber(client) > releaseNumber(target)) {
		return `A live Roblox client already runs ${client}, which upstream has not published yet: ${target} is the newest release there is to build.`;
	}
	return `A live Roblox client still runs ${client}${client === pinned ? ", the release the engine builds" : ""}: ${target} matters once a client runs it.`;
}

/** The issue for a plan: its title, and a body that is the same text for the same facts. */
export function renderIssue(behind, attempt) {
	const { pinned, target, spec, tagged } = behind;
	const links = tagged
		? `[${target}](${UPSTREAM}/releases/tag/${target}) ([changes since ${pinned}](${UPSTREAM}/compare/${pinned}...${target}))`
		: `${target} ([upstream's commits](${UPSTREAM}/commits/master))`;
	return {
		title: `Update Luau to ${target}`,
		body: [
			`Luau ${links} is out; the engine builds ${pinned} (\`patches/luau/upstream.json\`).`,
			"",
			clientLine(behind),
			"",
			"### The patch series",
			"",
			...seriesLines(behind, attempt),
			"",
			'### Steps (docs/building.md "Updating Luau")',
			"",
			`- [ ] \`node tools/vendor.mjs bump ${spec}\`; resolve each topic it stops at, then \`bump --continue\``,
			"- [ ] read upstream's changes to the files each topic touches: new opcodes, allocation paths and lookups can need a topic's work too",
			"- [ ] move `mlua` and `mlua-sys` if the C API they bind moved",
			"- [ ] `node tools/luau-flags.mjs`",
			"- [ ] `cargo build --release && cargo test --release`, and `node tools/vendor.mjs --check`",
			"- [ ] `node tools/parity/run.mjs --diff`",
			"- [ ] library functions and behaviour the release added are emulated, or report themselves as missing (docs/engine.md)",
			"- [ ] `tools/cost-parity`, THIRD_PARTY_NOTICES.md, and the pins of the crates that depend on this one",
			"",
			"This issue is kept by `tools/luau-release.mjs`: a newer release moves it, and it closes when the pin reaches the release.",
		].join("\n"),
	};
}

/**
 * What to do with the tracking issue. `issues` are the repository's open issues; `wanted` is the issue to have
 * ({ title, body }) or null when the pin is behind nothing.
 */
export function decide(issues, wanted) {
	const own = issues
		.filter(issue => !issue.pull_request && issue.user?.login === AUTHOR && TITLE.test(issue.title))
		.sort((a, b) => a.number - b.number);
	const [current, ...extra] = own;
	const actions = extra.map(issue => ({ type: "close", number: issue.number, comment: `#${current.number} tracks the Luau update.` }));
	if (wanted === null) {
		if (current) actions.push({ type: "close", number: current.number, comment: "The pinned Luau has reached this release." });
		return actions;
	}
	if (!current) return [{ type: "open", ...wanted }];
	if (current.title !== wanted.title) {
		const release = TITLE.exec(wanted.title)[1];
		actions.push({ type: "update", number: current.number, ...wanted, comment: `Luau ${release} is out; this issue now tracks it.` });
	} else if ((current.body ?? "").replace(/\r\n/g, "\n") !== wanted.body) {
		actions.push({ type: "update", number: current.number, ...wanted, comment: null });
	}
	return actions;
}

async function github(method, route, payload) {
	const response = await fetch(`https://api.github.com${route}`, {
		method,
		headers: {
			authorization: `Bearer ${process.env.GITHUB_TOKEN}`,
			accept: "application/vnd.github+json",
			"x-github-api-version": "2022-11-28",
			"user-agent": USER_AGENT,
		},
		body: payload === undefined ? undefined : JSON.stringify(payload),
	});
	if (!response.ok) throw new Error(`${method} ${route}: ${response.status} ${(await response.text()).slice(0, 300)}`);
	return response.json();
}

async function openIssues(repository) {
	const issues = [];
	for (let page = 1; page <= 10; page++) {
		const batch = await github("GET", `/repos/${repository}/issues?state=open&creator=${encodeURIComponent(AUTHOR)}&per_page=100&page=${page}`);
		issues.push(...batch);
		if (batch.length < 100) break;
	}
	return issues;
}

async function apply(repository, action) {
	const issue = `/repos/${repository}/issues`;
	if (action.type === "open") {
		const created = await github("POST", issue, { title: action.title, body: action.body });
		console.log(`opened #${created.number}: ${action.title}`);
		return;
	}
	if (action.comment) await github("POST", `${issue}/${action.number}/comments`, { body: action.comment });
	if (action.type === "update") {
		await github("PATCH", `${issue}/${action.number}`, { title: action.title, body: action.body });
		console.log(`updated #${action.number}: ${action.title}`);
	} else {
		await github("PATCH", `${issue}/${action.number}`, { state: "closed", state_reason: "completed" });
		console.log(`closed #${action.number}`);
	}
}

function upstreamTags() {
	const result = spawnSync("git", ["ls-remote", "--tags", `${UPSTREAM}.git`], { encoding: "utf8" });
	if (result.status !== 0) throw new Error(`git ls-remote ${UPSTREAM}: ${(result.stderr || "").trim()}`);
	const tags = parseTags(result.stdout);
	if (tags.length === 0) throw new Error(`git ls-remote ${UPSTREAM} listed no release tag`);
	return tags;
}

// null when the endpoint does not answer: the tags alone still say whether a release is out
async function liveClientRelease() {
	try {
		const response = await fetch(CLIENT_VERSION, { headers: { "user-agent": USER_AGENT } });
		if (!response.ok) throw new Error(`${response.status}`);
		return clientRelease((await response.json()).version);
	} catch (error) {
		console.error(`luau-release: the client's version could not be read (${error.message})`);
		return null;
	}
}

// Whether upstream's master has the commit that synced a release (it lands before the release's tag); false when
// that cannot be read, which leaves the release to the run that finds its tag
async function syncedUpstream(release) {
	const wanted = `Sync to upstream/release/${releaseNumber(release)}`;
	const headers = { accept: "application/vnd.github+json", "user-agent": USER_AGENT };
	if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
	try {
		for (let page = 1; page <= 3; page++) {
			const response = await fetch(`https://api.github.com/repos/luau-lang/luau/commits?per_page=100&page=${page}`, { headers });
			if (!response.ok) throw new Error(`${response.status}`);
			const commits = await response.json();
			if (commits.some(commit => commit.commit.message.split("\n")[0].startsWith(wanted))) return true;
			if (commits.length < 100) break;
		}
	} catch (error) {
		console.error(`luau-release: upstream's commits could not be read (${error.message})`);
	}
	return false;
}

async function main(argv) {
	const dryRun = argv.includes("--dry-run");
	const pinned = JSON.parse(fs.readFileSync(PIN, "utf8")).luau;
	const tags = upstreamTags();
	const client = await liveClientRelease();
	const unpublished = client !== null && releaseNumber(client) > releaseNumber(pinned) && !tags.includes(client);
	const synced = unpublished && (await syncedUpstream(client));
	if (unpublished && !synced) console.log(`a live client runs Luau ${client}, which upstream has not published yet`);
	const behind = plan({ pinned, tags, client, synced });
	let wanted = null;
	if (behind) {
		console.log(`the engine builds Luau ${pinned}; ${behind.target} is out`);
		const tried = spawnSync(process.execPath, [path.join(ROOT, "tools", "vendor.mjs"), "try", behind.spec], { cwd: ROOT, encoding: "utf8", maxBuffer: 64 << 20 });
		process.stdout.write(tried.stdout || "");
		wanted = renderIssue(behind, parseTry({ status: tried.status, stdout: tried.stdout || "", stderr: tried.stderr || "" }));
	} else {
		console.log(`the engine builds Luau ${pinned}, the newest release upstream has published`);
	}

	const repository = process.env.GITHUB_REPOSITORY;
	const online = Boolean(process.env.GITHUB_TOKEN && repository);
	if (!online && !dryRun) throw new Error("GITHUB_TOKEN and GITHUB_REPOSITORY say where the issue goes (or --dry-run)");
	const actions = decide(online ? await openIssues(repository) : [], wanted);
	if (actions.length === 0) console.log("the tracking issue is as it should be");
	for (const action of actions) {
		if (!dryRun) {
			await apply(repository, action);
			continue;
		}
		console.log(`would ${action.type}${action.number ? ` #${action.number}` : ""}${action.title ? `: ${action.title}` : ""}`);
		if (action.comment) console.log(`  comment: ${action.comment}`);
		if (action.body) console.log(`\n${action.body}\n`);
	}
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	try {
		await main(process.argv.slice(2));
	} catch (error) {
		console.error(`luau-release: ${error.message}`);
		process.exitCode = 2;
	}
}

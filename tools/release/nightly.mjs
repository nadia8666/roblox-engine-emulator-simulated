// Decides whether the nightly release run builds main's head. It builds and smoke-tests every platform, as a push dry
// run does, unless nothing that ships changed since the newest commit that a passing run built:
//
//   node tools/release/nightly.mjs --repository <owner/name> --sha <head commit> --artifacts <name,name,...> [--branch <default branch>]
//
// The base is the newest commit of the head's first-parent history with a run that a tag could publish from: a passing
// run of the release workflow on the default branch, started by a push, by hand or by the nightly schedule, that still
// has an unexpired artifact of every name given (dry-run.mjs's choice and lookups, so the two never differ). Then:
//   - no base: build;
//   - the base is the head: build nothing, the head was built already;
//   - otherwise build when a path that ships is among `git diff --name-only <base> <head>`, and build nothing when none
//     is. A renamed file counts as its old path and its new one, so a file moved out of the shipped paths still counts.
// Prints `build=true` or `build=false` and `reason=<one line>` for $GITHUB_OUTPUT; the workflow writes the reason to the
// run's summary. Says on stderr which runs it passed over. Reads the history and the head's workflow file from the
// checkout in the current folder, which needs the full history, and the GitHub API with GH_TOKEN (or GITHUB_TOKEN),
// which needs `actions: read`. Any failure to decide builds, with a warning on the run's page. Node.js only, no npm
// packages; the same file in each tool repository.
//
// A path ships by the rule that decides when a release is due, so the nightly build and the releases agree:
//   - a path under .github/ does not ship;
//   - else a path that matches one of the workflow's own on.push.paths globs ships;
//   - else a path with a part that starts with "." does not ship;
//   - else it ships unless it matches one of NOT_SHIPPED.
// In a glob, `**` stands for any number of path segments, `*` and `?` stay within one segment, `{a,b}` matches either,
// and a trailing `/` means everything below.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import { WORKFLOW, pickDryRun, repositoryApi } from "./dry-run.mjs";

// What a release does not ship unless the workflow lists it: CI, docs and notes, tests, benches, examples, fixtures and
// goldens, the repository's own tools, and Markdown
export const NOT_SHIPPED = [".github/**", "**/*.md", "docs/**", "notes/**", "**/tests/**", "**/benches/**", "**/examples/**", "**/fixtures/**", "**/goldens/**", "tools/**"];
// The passing runs looked at: at most this many pages of 100, newest first
const RUN_PAGES = 3;
// The runs checked job by job and artifact by artifact before the search gives up and the run builds
const CHECKED_RUNS = 50;
// The changed paths that ship named in the reason
const NAMED_PATHS = 10;
const USAGE = "usage: nightly.mjs --repository <owner/name> --sha <head commit> --artifacts <name,...> [--branch <default branch>]";

// The `on.push.paths` globs of a workflow file: the block list below its first `paths:` key, one item per line, plain
// or quoted, with comment lines between them.
export function pushPaths(text) {
	const lines = text.split(/\r?\n/);
	const indent = (line) => line.length - line.trimStart().length;
	const at = lines.findIndex((line) => /^\s*paths:\s*(?:#.*)?$/.test(line));
	if (at === -1) return [];
	const own = indent(lines[at]);
	const paths = [];
	for (const line of lines.slice(at + 1)) {
		if (!line.trim() || line.trim().startsWith("#")) continue;
		if (indent(line) <= own && !line.trimStart().startsWith("-")) break;
		const item = /^\s*-\s*(.+?)\s*(?:\s#.*)?$/.exec(line);
		if (!item) break;
		paths.push(item[1].replace(/^(['"])(.*)\1$/, "$2"));
	}
	return paths;
}

const escapeRegex = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

// A glob over repository paths with forward slashes, as a RegExp
export function globRegex(glob) {
	const pattern = glob.endsWith("/") ? `${glob}**` : glob;
	let re = "";
	for (let i = 0; i < pattern.length; i++) {
		const c = pattern[i];
		if (c === "*" && pattern[i + 1] === "*") {
			// `**/` at the start or after a slash is any number of whole segments, none included; elsewhere anything
			if ((i === 0 || pattern[i - 1] === "/") && pattern[i + 2] === "/") {
				re += "(?:.*/)?";
				i += 2;
			} else {
				re += ".*";
				i += 1;
			}
		} else if (c === "*") {
			re += "[^/]*";
		} else if (c === "?") {
			re += "[^/]";
		} else if (c === "{") {
			const end = pattern.indexOf("}", i);
			if (end === -1) throw new Error(`unclosed { in glob ${glob}`);
			re += `(?:${pattern.slice(i + 1, end).split(",").map(escapeRegex).join("|")})`;
			i = end;
		} else {
			re += escapeRegex(c);
		}
	}
	return new RegExp(`^${re}$`);
}

function matcher(globs) {
	const regexes = globs.map(globRegex);
	return (path) => regexes.some((re) => re.test(path));
}

// (path) => whether a change to it ships, given the workflow's on.push.paths globs
export function shipsFrom(listed) {
	const isListed = matcher(listed);
	const notShipped = matcher(NOT_SHIPPED);
	return (path) => {
		if (path.startsWith(".github/")) return false;
		if (isListed(path)) return true;
		if (path.split("/").some((part) => part.startsWith("."))) return false;
		return !notShipped(path);
	};
}

// The decision for `head`: { build, reason, base, run, shipping }. `history` is the head's first-parent history, newest
// first and the head first; `passingRun(sha)` the run at `sha` a tag could publish from, or null; `changed(base, head)`
// the paths the diff between them names; `ships(path)` the rule above.
export async function decide({ head, history, passingRun, changed, ships, runUrl = (run) => run.html_url }) {
	for (const sha of history) {
		const run = await passingRun(sha);
		if (!run) continue;
		const by = `built by run ${runUrl(run)}`;
		if (sha === head) return { build: false, reason: `head already ${by}`, base: sha, run, shipping: [] };
		const shipping = changed(sha, head).filter(ships);
		if (shipping.length === 0) return { build: false, reason: `nothing that ships changed since ${sha}, ${by}`, base: sha, run, shipping };
		const named = shipping.slice(0, NAMED_PATHS).join(", ") + (shipping.length > NAMED_PATHS ? `, and ${shipping.length - NAMED_PATHS} more` : "");
		return { build: true, reason: `builds every platform at ${head}: ${shipping.length} changed path(s) that ship since ${sha}, ${by}: ${named}`, base: sha, run, shipping };
	}
	return { build: true, reason: `builds every platform at ${head}: no commit of its first-parent history has a passing run that kept every zip`, base: null, run: null, shipping: [] };
}

function options(argv) {
	const result = {};
	for (let i = 0; i < argv.length; i++) {
		const name = argv[i]?.replace(/^--/, "");
		if (!["repository", "sha", "artifacts", "branch"].includes(name) || argv[i + 1] === undefined || name in result) throw new Error(USAGE);
		result[name] = argv[++i];
	}
	if (!result.repository || !/^[0-9a-f]{40}$/.test(result.sha ?? "") || !result.artifacts) throw new Error("--repository, a full --sha and --artifacts are required");
	return result;
}

const oneLine = (text) => text.replace(/\s*[\r\n]+\s*/g, " ");

// Reads the history in `cwd` and the runs through the API, and gives the lines for $GITHUB_OUTPUT
export async function run(argv, env = process.env, log = console.error, cwd = process.cwd()) {
	const { repository, sha: head, artifacts, branch = "main" } = options(argv);
	const git = (args) => execFileSync("git", args, { cwd, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
	const history = git(["rev-list", "--first-parent", head]).split("\n").filter(Boolean);
	const ships = shipsFrom(pushPaths(git(["show", `${head}:${WORKFLOW}`])));
	const { get, jobsOf, artifactsOf } = repositoryApi(repository, env);

	// the passing runs of the release workflow on the branch, by commit
	const byCommit = new Map();
	for (let page = 1; page <= RUN_PAGES; page++) {
		const runs = (await get(`/actions/workflows/release.yml/runs?branch=${encodeURIComponent(branch)}&status=success&exclude_pull_requests=true&per_page=100&page=${page}`)).workflow_runs ?? [];
		for (const r of runs) byCommit.set(r.head_sha, [...(byCommit.get(r.head_sha) ?? []), r]);
		if (runs.length < 100) break;
	}
	const names = artifacts.split(",").map((name) => name.trim()).filter(Boolean);
	let checked = 0;
	const passingRun = async (sha) => {
		const runs = byCommit.get(sha);
		if (!runs) return null;
		if (checked >= CHECKED_RUNS) return null;
		checked += runs.length;
		const { run: found, reasons } = await pickDryRun({ runs, jobsOf, artifactsOf, sha, branch, artifacts: names });
		for (const reason of reasons) log(`passed over ${reason}`);
		if (!found && checked >= CHECKED_RUNS) log(`checked ${checked} passing runs without finding one that kept every zip; looking no further`);
		return found;
	};
	const changed = (base, to) => git(["diff", "--name-only", "--no-renames", "-z", base, to]).split("\0").filter(Boolean);
	const server = env.GITHUB_SERVER_URL || "https://github.com";
	const decision = await decide({ head, history, passingRun, changed, ships, runUrl: (r) => r.html_url ?? `${server}/${repository}/actions/runs/${r.id}` });
	log(decision.reason);
	return `build=${decision.build}\nreason=${oneLine(decision.reason)}`;
}

// Started as the script, compared by real path so that a start through a linked folder counts too
function startedAsScript() {
	try {
		return Boolean(process.argv[1]) && fs.realpathSync.native(process.argv[1]) === fs.realpathSync.native(fileURLToPath(import.meta.url));
	} catch {
		return false;
	}
}

// A bad command line fails the step. Anything else that keeps it from deciding builds, which can only cost a build, and
// the warning shows on the run's page.
if (startedAsScript()) {
	try {
		options(process.argv.slice(2));
		run(process.argv.slice(2)).then((lines) => console.log(lines), (error) => {
			const reason = oneLine(`builds every platform: could not tell whether anything that ships changed since a passing run: ${error.message}`);
			console.error(`::warning::${reason}`);
			console.log(`build=true\nreason=${reason}`);
		});
	} catch (error) {
		console.error(error.message);
		process.exitCode = 1;
	}
}

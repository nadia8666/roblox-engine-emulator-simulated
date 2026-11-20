// The release notes of a tag: the public commit subjects since the previous release, how to install and verify the
// release, and where the source is. With --canary, the notes of the canary prerelease instead: a build of main, when
// its zips were built, and the commits since the latest version release.
//
//   node tools/release/notes.mjs --tag v<version> --out <file>
//   node tools/release/notes.mjs --canary <commit> --built <ISO time> --out <file>
//
// It needs a full-history checkout that has the tags. Subjects are read through git's argument array and written
// to a file, never put into a shell command. Only vMAJOR.MINOR.PATCH tags count as releases, so canary never does.
import fs from "node:fs";
import { PLATFORMS, REPOSITORY, args, command, isMain, main, sourceText, version } from "./common.mjs";

const FENCE = "```";
const RELEASE_TAG = /^v\d+\.\d+\.\d+$/;
// The most commit subjects the canary's notes list; the compare link has them all
const CANARY_LIST = 50;

// The release before `tag` among the releases it contains (vMAJOR.MINOR.PATCH tags, newest first), if there is one
function previousRelease(tag, run) {
	const tags = run("git", ["tag", "--merged", tag, "--sort=-version:refname"]);
	return tags.split(/\r?\n/).find((other) => other !== tag && RELEASE_TAG.test(other));
}

// The newest release `commit` contains, itself included, if there is one
function latestRelease(commit, run) {
	const tags = run("git", ["tag", "--merged", commit, "--sort=-version:refname"]);
	return tags.split(/\r?\n/).find((tag) => RELEASE_TAG.test(tag));
}

// The canary's asset names, ree-canary-<os>-<arch>.zip, the same from one build to the next
export function canaryAssets() {
	return Object.entries(PLATFORMS).map(([os, { arch }]) => `ree-canary-${os}-${arch}.zip`);
}

// The canary's notes: the commit, when the run that built its zips started (`built`, an ISO time), the commits since
// the latest release, newest first, the download links and where the source is
export function canaryNotes(commit, built, run = command) {
	if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error("canary commit must be a full SHA");
	const time = new Date(built);
	if (!built || Number.isNaN(time.getTime())) throw new Error(`not a build time: ${built}`);
	const site = `https://github.com/${REPOSITORY}`;
	const latest = latestRelease(commit, run);
	const subjects = run("git", ["log", "--format=%s", latest ? `${latest}..${commit}` : commit]).trim().split(/\r?\n/).filter(Boolean);
	const count = `${subjects.length} commit${subjects.length === 1 ? "" : "s"}`;
	const since = latest ? `${count} since ${latest} ([compare](${site}/compare/${latest}...${commit}))` : `${count}, no release yet`;
	const listed = subjects.slice(0, CANARY_LIST).map((subject) => `- ${subject}`);
	if (subjects.length > CANARY_LIST) listed.push(`- and ${subjects.length - CANARY_LIST} earlier`);
	return [
		`A build of main, not a release: it may break. Releases are at ${site}/releases.`,
		"",
		`- Commit: [${commit.slice(0, 10)}](${site}/commit/${commit})`,
		`- Built: ${time.toISOString().slice(0, 16).replace("T", " ")} UTC`,
		`- ${since}${listed.length ? ":" : ""}`,
		"",
		...(listed.length ? [...listed, ""] : []),
		"## Downloading",
		"",
		"The zips keep these names from one build to the next, so a link always gives the newest canary:",
		"",
		...canaryAssets().map((name) => `- ${site}/releases/download/canary/${name}`),
		"",
		"Check the zip's SHA-256 against SHA256SUMS. No Studio installation or GPU is needed for offline harnesses.",
		"",
		"## Source",
		"",
		"This binary is distributed under GPL-3.0-or-later. Its source is",
		`${site}/tree/${commit}, and GitHub's source archive ${site}/archive/${commit}.tar.gz.`,
		"SOURCE.md in each zip names the same commit, the pinned dependencies and the build command.",
		"",
	].join("\n");
}

// One "- <subject>" line per commit, oldest first; all commits up to the tag for the first release
function changeList(tag, previous, run) {
	const range = previous ? `${previous}..${tag}` : tag;
	const subjects = run("git", ["log", "--reverse", "--format=%s", range]);
	return subjects.trim().split(/\r?\n/).filter(Boolean).map((subject) => `- ${subject}`).join("\n");
}

export function notes(releaseVersion, run = command) {
	const tag = `v${releaseVersion}`;
	const commit = run("git", ["rev-parse", `${tag}^{commit}`]).trim();
	const changes = changeList(tag, previousRelease(tag, run), run);
	return [
		`# ree ${tag}`,
		"",
		changes,
		"",
		"## Installing",
		"",
		"Download the zip for your platform and put ree on PATH, or add this to rokit.toml:",
		"",
		`${FENCE}toml`,
		`ree = "${REPOSITORY}@${releaseVersion}"`,
		FENCE,
		"",
		"Use rokit 1.0 or later on Linux and macOS; rokit 0.2.x cannot select the differently named executable there.",
		"No Studio installation or GPU is needed for offline harnesses. See docs/building.md for runtime requirements.",
		"",
		"## Verifying",
		"",
		"Check the zip's SHA-256 against SHA256SUMS, then run:",
		"",
		`${FENCE}sh`,
		`gh attestation verify <zip> -R ${REPOSITORY}`,
		FENCE,
		"",
		sourceText(releaseVersion, commit),
	].join("\n");
}

if (isMain(import.meta.url)) main(() => {
	const options = args(process.argv.slice(2), ["tag", "out", "canary", "built"]);
	const usage = "usage: notes.mjs --tag v<version> --out <file>, or --canary <commit> --built <ISO time> --out <file>";
	if (options.canary) {
		if (options.tag || !options.built || !options.out) throw new Error(usage);
		fs.writeFileSync(options.out, canaryNotes(options.canary, options.built));
		return;
	}
	const releaseVersion = version();
	if (options.tag !== `v${releaseVersion}` || !options.out || options.built) throw new Error(usage);
	fs.writeFileSync(options.out, notes(releaseVersion));
});

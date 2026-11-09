// The release notes of a tag: the public commit subjects since the previous release, how to install and verify the
// release, and where the source is.
//
//   node tools/release/notes.mjs --tag v<version> --out <file>
//
// It needs a full-history checkout that has the tag. Subjects are read through git's argument array and written
// to a file, never put into a shell command.
import fs from "node:fs";
import { REPOSITORY, args, command, isMain, main, sourceText, version } from "./common.mjs";

const FENCE = "```";

// The release before `tag` among the releases it contains (vMAJOR.MINOR.PATCH tags, newest first), if there is one
function previousRelease(tag, run) {
	const tags = run("git", ["tag", "--merged", tag, "--sort=-version:refname"]);
	return tags.split(/\r?\n/).find((other) => other !== tag && /^v\d+\.\d+\.\d+$/.test(other));
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
	const options = args(process.argv.slice(2), ["tag", "out"]);
	const releaseVersion = version();
	if (options.tag !== `v${releaseVersion}` || !options.out) throw new Error("usage: notes.mjs --tag v<version> --out <file>");
	fs.writeFileSync(options.out, notes(releaseVersion));
});

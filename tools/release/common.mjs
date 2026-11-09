// What the release scripts share: the repository's facts (binary, platforms, version), the command line, running a
// program, and the text of SOURCE.md. Node.js only, no npm packages.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
export const BINARY = "ree";

// The manifests whose versions must agree (the hardware-counter service has its own version and is not shipped)
export const MANIFESTS = ["Cargo.toml", "profile-view/Cargo.toml"];

// The platforms that ship, by the word used in the archive name
export const PLATFORMS = {
	windows: { arch: "x86_64", target: "x86_64-pc-windows-msvc", exe: "ree.exe" },
	linux: { arch: "x86_64", target: "x86_64-unknown-linux-gnu", exe: "ree" },
	macos: { arch: "aarch64", target: "aarch64-apple-darwin", exe: "ree" },
};

// The public repository as owner/name, from the `repository` URL in Cargo.toml
export const REPOSITORY = new URL(packageFields(fs.readFileSync(path.join(ROOT, "Cargo.toml"), "utf8")).repository).pathname.slice(1);

// Whether the module at `moduleUrl` (import.meta.url) was started as the script, rather than imported by a test.
// The two are compared by real path: started through a linked folder (a symlink, or a junction on Windows), Node names
// the module by its real path and `process.argv[1]` by the linked one, so comparing the spellings never matches.
export function isMain(moduleUrl) {
	if (!process.argv[1]) return false;
	const script = path.resolve(process.argv[1]);
	try {
		return fs.realpathSync(fileURLToPath(moduleUrl)) === fs.realpathSync(script);
	} catch {
		// a path that does not exist has no real path to resolve, so only the plain spellings can match
		return moduleUrl === pathToFileURL(script).href;
	}
}

// Runs `fn`; a failure prints its message and sets the exit code
export async function main(fn) {
	try {
		await fn();
	} catch (error) {
		console.error(error.message);
		process.exitCode = 1;
	}
}

// Reads `--name value` pairs. Every name must be one of `names`, appear once and have a value.
export function args(argv, names) {
	const result = {};
	for (let i = 0; i < argv.length; i++) {
		const key = argv[i].replace(/^--/, "");
		const value = argv[i + 1];
		const known = argv[i].startsWith("--") && names.includes(key);
		if (!known || !value || value.startsWith("--") || key in result) {
			throw new Error(`unexpected or incomplete argument: ${argv[i]}`);
		}
		result[key] = value;
		i++;
	}
	return result;
}

// Runs a program in the repository root and returns what it printed; fails with its output if it exits with an error
export function command(program, argv, options = {}) {
	const result = spawnSync(program, argv, { cwd: ROOT, encoding: "utf8", maxBuffer: 128 * 1024 * 1024, windowsHide: true, ...options });
	const name = `${program} ${argv.join(" ")}`;
	if (result.error) throw new Error(`${name}: ${result.error.message}`);
	if (result.status !== 0) throw new Error(`${name}: exit ${result.status}: ${result.stderr || result.stdout}`);
	return result.stdout;
}

// `name`, `version` and `repository` of a manifest's [package] table. Cargo.lock's [[package]] tables read the same.
export function packageFields(text) {
	const table = text.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
	if (!table) throw new Error("manifest has no [package]");
	const field = (name) => table.match(new RegExp(`^${name}\\s*=\\s*"([^"]+)"`, "m"))?.[1];
	return { name: field("name"), version: field("version"), repository: field("repository") };
}

// The version `package` has in Cargo.lock, if it has an entry
function lockedVersion(lockText, packageName) {
	const tables = lockText.split(/^\[\[package\]\]\s*$/m).slice(1);
	const entries = tables.map((table) => packageFields(`[package]\n${table}`));
	return entries.find((entry) => entry.name === packageName)?.version;
}

// The release version: the manifests and Cargo.lock must all agree on it
export function version(root = ROOT) {
	const packages = MANIFESTS.map((file) => packageFields(fs.readFileSync(path.join(root, file), "utf8")));
	const releaseVersion = packages[0].version;
	const agree = packages.every((pkg) => pkg.version === releaseVersion);
	if (!/^\d+\.\d+\.\d+$/.test(releaseVersion) || !agree) {
		throw new Error("release manifest versions must agree and be MAJOR.MINOR.PATCH");
	}
	const lock = fs.readFileSync(path.join(root, "Cargo.lock"), "utf8");
	for (const pkg of packages) {
		if (lockedVersion(lock, pkg.name) !== releaseVersion) throw new Error(`Cargo.lock disagrees with ${pkg.name} ${releaseVersion}`);
	}
	return releaseVersion;
}

// The git dependencies Cargo.lock pins, as "- <repository URL>, commit <SHA>." lines
function gitDependencyLines() {
	const lock = fs.readFileSync(path.join(ROOT, "Cargo.lock"), "utf8");
	const pins = [...lock.matchAll(/^source = "git\+([^"#]+)#([0-9a-f]+)"/gm)];
	// a source URL can end in "?rev=..." or "?branch=..."; the commit after "#" is what counts
	const lines = pins.map(([, url, commit]) => `- ${url.split("?")[0]}, commit ${commit}.`);
	return [...new Set(lines)];
}

// SOURCE.md: where this binary's Corresponding Source is (GPL-3.0-or-later asks for it to stay reachable)
export function sourceText(releaseVersion, commit, repository = REPOSITORY) {
	if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error("source commit must be a full SHA");
	if (repository !== REPOSITORY) throw new Error("release source must be the public repository");
	const tag = `v${releaseVersion}`;
	return [
		"# Corresponding Source",
		"",
		"This binary is distributed under GPL-3.0-or-later.",
		"",
		`The source is https://github.com/${repository}/tree/${tag}, commit ${commit}.`,
		`GitHub's source archive: https://github.com/${repository}/archive/refs/tags/${tag}.tar.gz`,
		"",
		"Cargo.lock pins the Rust crates from crates.io, with each archive's SHA-256 checksum.",
		"Git dependencies are available in their repositories at the pinned commits:",
		...gitDependencyLines(),
		"",
		"The patched Luau VM and the other vendored sources are included in this repository.",
		"Build with Rust 1.90.0 and a C++ toolchain: node tools/vendor.mjs --check, then",
		"cargo build --release --locked --bin ree --target <target triple>, using the default features.",
		'For Windows set RUSTFLAGS="-C target-feature=+crt-static" to link the C/C++ runtime into the executable,',
		`and CXXFLAGS_x86_64_pc_windows_msvc="-DMI_WIN_INIT_USE_FLS=1" for mimalloc's initialization after the CRT.`,
		"",
	].join("\n");
}

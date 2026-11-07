// The patched Luau VM and Lune datatypes the engine builds against, vendored in vendor/ so that a
// plain `cargo build` (here, or in a crate that depends on this one) needs nothing else:
//
//   vendor/luau0-src      the Luau release pinned in patches/luau/upstream.json (the sources mlua-sys compiles, with
//                         luau0-src's build logic) and the topic series patches/luau/NN-<topic>.patch
//   vendor/mlua-sys       mlua-sys, the FFI layer under mlua, as Cargo.toml pins it, with the vendored Luau's bytecode
//                         version range
//   vendor/lune-roblox    Lune 0.10.4's lune-roblox (Roblox datatypes as Luau values) with patches/lune.patch
//   vendor/lune-utils     its lune-utils, with the same patch
//
//   node tools/vendor.mjs                       rebuild vendor/ from the pins and the patches
//   node tools/vendor.mjs --check               vendor/ is exactly the pins with the patches (fails otherwise)
//   node tools/vendor.mjs bump <target> [--crate <luau0-src version>] [--verify]
//                                               move the pinned Luau to another release (docs/building.md "Updating Luau")
//   node tools/vendor.mjs bump --continue|--abort
//   node tools/vendor.mjs try <target> [--keep] whether the series applies to a release; changes nothing
//   node tools/vendor.mjs edit [--finish]       change a topic in the middle of the series: the series as commits on the
//                                               pinned release, fixups folded into their topics, the patches rewritten
//
// A target is `client` (the release a live Roblox client runs: its tag, or before the tag exists upstream's "Sync to
// upstream/release/NNN" commit), `latest` (the newest release tag), a tag (0.746) or a commit of luau-lang/luau.
//
// The series: each patch is one topic, applied in file-name order, with a `#` header saying what it does and why upstream
// lacks it. A backport says `# Upstream: <release>`, the first release that has the change with no flag; `bump` leaves it
// out once the target is that release or later. `bump` rebases the rest onto the target with git's three-way merge, one
// topic at a time, and stops at a topic that conflicts, naming its files and lines; once they are resolved,
// `bump --continue` goes on. The patches carry their history from roblox-ui-renderer's tools/native-host.
//
// The vendored manifests lose `[lints] workspace = true` (Lune's workspace lints, not ours) and take the engine's mlua
// version, and each Lune crate gets Lune's license text (MPL-2.0, the archive's LICENSE.txt) beside it, since its files
// are modified copies. luau0-src's crate carries its own LICENSE (MIT: Luau's and Lua's notices); mlua-sys's has none, so
// mlua's (the same repository's MIT license) goes beside it. The VM's ClientFlagValues.h is written from
// data/luau-flags.json (tools/luau-flags.mjs).
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const VENDOR = path.join(ROOT, "vendor");
const CACHE = path.join(ROOT, ".cache", "vendor");
const SERIES = path.join(ROOT, "patches", "luau");
const PIN = path.join(SERIES, "upstream.json");
const BUMP = path.join(CACHE, "bump");
const UPSTREAM = "https://github.com/luau-lang/luau";
const USER_AGENT = "ree-vendor (+https://github.com/Anime-Vanguards/roblox-engine-emulator)";

// Windows' own bsdtar: Git Bash puts GNU tar first on PATH, which reads "C:\..." as a remote host
const TAR = process.platform === "win32" && fs.existsSync(path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe"))
	? path.join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe") : "tar";

const LUNE = {
	file: "lune-0.10.4.tar.gz",
	url: "https://github.com/lune-org/lune/archive/refs/tags/v0.10.4.tar.gz",
	sha256: "151b7b738b210920297b8afa560f440877662e02a48c64c28ce95baaefc96c95",
};

// mlua-sys tells Luau bytecode from source text by the chunk's version byte, checked against its own copy of Bytecode.h's
// range, so it reads the bytecode of a Luau newer than the one it binds as text (and mlua refuses it in binary mode). The
// vendored copy takes the range of the Luau it builds. The mlua crate supplies the license text mlua-sys's crate lacks.
const MLUA_SYS = {
	file: "mlua-sys-0.13.0.crate",
	url: "https://static.crates.io/crates/mlua-sys/mlua-sys-0.13.0.crate",
	sha256: "1631cda17bd6486410ab751d91f5fd4484f441589278cdad879eb3afc721a285",
};
const MLUA = {
	file: "mlua-0.12.2.crate",
	url: "https://static.crates.io/crates/mlua/mlua-0.12.2.crate",
	sha256: "7a7ae6b65bf54b7fdd4cf5b6f44f60862350ebf3fe3779b3a9cc005e7fa14f81",
};

// Directories of a Luau release that luau0-src never builds (the type checker, the command line tools, tests)
const UNBUILT = new Set(["Analysis", "CLI", "EqSat", "bench", "extern", "fuzz", "tests", "tools"]);

const digest = bytes => createHash("sha256").update(bytes).digest("hex");

function run(command, parameters, cwd, input) {
	const result = spawnSync(command, parameters, { cwd, input, stdio: [input === undefined ? "ignore" : "pipe", "inherit", "inherit"], windowsHide: true });
	if (result.error || result.status !== 0) throw result.error || new Error(`${command} ${parameters.join(" ")} exited ${result.status}`);
}

function capture(command, parameters, cwd, input) {
	const result = spawnSync(command, parameters, { cwd, input, encoding: "utf8", maxBuffer: 1 << 28, windowsHide: true });
	if (result.error) throw result.error;
	return result;
}

async function download(url) {
	const response = await fetch(url, { headers: { "User-Agent": USER_AGENT } });
	if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
	return Buffer.from(await response.arrayBuffer());
}

// A pinned archive in the cache, downloaded when missing (and checked against its SHA-256 either way)
async function archive({ file, url, sha256 }) {
	fs.mkdirSync(CACHE, { recursive: true });
	const target = path.join(CACHE, file);
	if (!fs.existsSync(target) || digest(fs.readFileSync(target)) !== sha256) {
		console.error(`vendor: downloading ${url}`);
		const bytes = await download(url);
		if (digest(bytes) !== sha256) throw new Error(`${file}: checksum mismatch`);
		fs.writeFileSync(target, bytes);
	}
	return target;
}

const releaseNumber = release => Number(/^0\.(\d+)$/.exec(release)?.[1] ?? NaN);

const readPin = () => JSON.parse(fs.readFileSync(PIN, "utf8"));

function writePin(pin) {
	fs.writeFileSync(PIN, JSON.stringify(pin, null, "\t") + "\n");
}

// The topics, in order: { name, file, header, body, upstream }
function readSeries() {
	return fs.readdirSync(SERIES).filter(name => /^\d+-[\w.-]+\.patch$/.test(name)).sort().map(name => {
		const file = path.join(SERIES, name);
		const text = fs.readFileSync(file, "utf8").replaceAll("\r\n", "\n");
		const start = text.search(/^(diff --git |--- )/m);
		if (start < 0) throw new Error(`${name}: no diff in it`);
		const header = text.slice(0, start);
		const upstream = /^# Upstream: (0\.\d+)/m.exec(header)?.[1] ?? null;
		return { name: name.replace(/\.patch$/, ""), file, header, body: text.slice(start), upstream };
	});
}

function applyPatchText(text, cwd, name) {
	// autocrlf off: with core.autocrlf=true (Git for Windows' default) git apply rewrites every file it patches to CRLF,
	// and the archives are LF, so --check failed on any Windows checkout
	const result = spawnSync("git", ["-c", "core.autocrlf=false", "apply", "--whitespace=nowarn", "-"], { cwd, input: text.replaceAll("\r\n", "\n"), encoding: "utf8", windowsHide: true });
	if (result.error || result.status !== 0) throw result.error || new Error(`${name} does not apply: ${result.stderr.trim()}`);
}

// The unpatched luau0-src tree of a pin: the crate (its build logic, its Custom/ additions to the C API) and, when the pin
// names an upstream source archive, that release's library directories in place of the crate's, with the release in
// the crate version's build metadata (mlua-sys reads it for LUAU_VERSION)
async function luauTree(pin, out) {
	const crate = await archive(pin.crate);
	fs.mkdirSync(out, { recursive: true });
	run(TAR, ["-xzf", crate, "--strip-components=1", "-C", out], out);
	if (!pin.source)
		return;
	const source = await archive(pin.source);
	const upstream = fs.mkdtempSync(path.join(path.dirname(out), "upstream-"));
	try {
		run(TAR, ["-xzf", source, "--strip-components=1", "-C", upstream], upstream);
		const built = fs.readdirSync(path.join(out, "luau"), { withFileTypes: true }).filter(entry => entry.isDirectory()).map(entry => entry.name);
		for (const name of built) {
			if (name === "Custom")
				continue;
			const from = path.join(upstream, name);
			if (!fs.existsSync(from))
				throw new Error(`Luau ${pin.luau} has no ${name}/, which the luau0-src crate ${pin.crate.file} builds: pin a crate made for this layout (--crate)`);
			fs.rmSync(path.join(out, "luau", name), { recursive: true, force: true });
			fs.cpSync(from, path.join(out, "luau", name), { recursive: true });
		}
		for (const entry of fs.readdirSync(upstream, { withFileTypes: true })) {
			if (entry.isDirectory() && !built.includes(entry.name) && !UNBUILT.has(entry.name) && fs.existsSync(path.join(upstream, entry.name, "src")))
				console.error(`vendor: Luau ${pin.luau} has a library directory ${entry.name}/ that the crate does not build`);
		}
		// the release's own notices beside its sources (the sources point at LICENSE.txt; the crate's LICENSE lags them)
		for (const notice of ["LICENSE.txt", "lua_LICENSE.txt"])
			fs.copyFileSync(path.join(upstream, notice), path.join(out, "luau", notice));
	} finally {
		fs.rmSync(upstream, { recursive: true, force: true });
	}
	for (const manifest of ["Cargo.toml", "Cargo.toml.orig"]) {
		const file = path.join(out, manifest);
		if (fs.existsSync(file))
			fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace(/^version = "[^"]*"/m, `version = "${pin.version}"`));
	}
}

function stripWorkspaceLints(manifest) {
	const text = fs.readFileSync(manifest, "utf8");
	fs.writeFileSync(manifest, text.replace(/\n\[lints\]\nworkspace = true\n/, "\n"));
}

// A vendored Lune crate builds against the engine's mlua (Cargo.toml's exact version), whatever Lune's release asked for
function pinMlua(manifest) {
	const engine = /^mlua = \{ version = "([^"]+)"/m.exec(fs.readFileSync(path.join(ROOT, "Cargo.toml"), "utf8"))?.[1];
	if (!engine) throw new Error("Cargo.toml names no mlua version");
	const text = fs.readFileSync(manifest, "utf8");
	if (!/^mlua = \{ version = "[^"]*"/m.test(text)) throw new Error(`${manifest}: no mlua dependency`);
	fs.writeFileSync(manifest, text.replace(/^(mlua = \{ version = )"[^"]*"/m, (_, start) => `${start}"${engine}"`));
}

// The client's flag values (data/luau-flags.json, tools/luau-flags.mjs) as the VM's ClientFlagValues.h, when the series has
// the topic that applies them (14-client-flags)
const FLAGS = path.join(ROOT, "data", "luau-flags.json");
const FLAG_VALUES = path.join("luau", "Common", "include", "Luau", "ClientFlagValues.h");

function writeFlagValues(luau) {
	const target = path.join(luau, FLAG_VALUES);
	if (!fs.existsSync(target) || !fs.existsSync(FLAGS))
		return;
	const data = JSON.parse(fs.readFileSync(FLAGS, "utf8"));
	const lines = Object.entries(data.flags).map(([key, value]) => {
		const match = /^(D?)F(Flag|Int)(\w+)$/.exec(key);
		if (!match) throw new Error(`data/luau-flags.json: a flag name this tool does not read: ${key}`);
		const int = match[2] === "Int";
		if (int ? !Number.isInteger(value) : typeof value !== "boolean") throw new Error(`data/luau-flags.json: ${key} has the wrong kind of value`);
		return `LUAU_CLIENT_FLAG(${match[3]}, ${match[1] ? "true" : "false"}, ${int}, ${int ? value : value ? 1 : 0})`;
	});
	fs.writeFileSync(target,
		"// The flag values ClientFlags.h applies, one LUAU_CLIENT_FLAG(name, dynamic, isint, value) a flag: what a live Roblox client\n" +
		`// (${data.client}) served on ${data.fetched}. Written by the engine's tools/vendor.mjs from its data/luau-flags.json.\n` +
		lines.join("\n") + "\n");
}

// vendor/mlua-sys: the crate Cargo.toml pins, its bytecode version range the vendored Luau's (`luau`, the luau0-src tree)
async function mluaSysTree(luau, out) {
	const pinned = /^mlua-sys = "=([^"]+)"/m.exec(fs.readFileSync(path.join(ROOT, "Cargo.toml"), "utf8"))?.[1];
	const version = /^mlua-sys-(.+)\.crate$/.exec(MLUA_SYS.file)[1];
	if (pinned !== version) throw new Error(`Cargo.toml pins mlua-sys ${pinned ?? "(no exact version)"} but tools/vendor.mjs vendors ${version}: move MLUA_SYS with it`);
	fs.mkdirSync(out, { recursive: true });
	run(TAR, ["-xzf", await archive(MLUA_SYS), "--strip-components=1", "-C", out], out);
	run(TAR, ["-xzf", await archive(MLUA), "--strip-components=1", "-C", out, `${MLUA.file.replace(/\.crate$/, "")}/LICENSE`], out);
	// cargo never reads a dependency's lockfile
	fs.rmSync(path.join(out, "Cargo.lock"), { force: true });
	const header = fs.readFileSync(path.join(luau, "luau", "Common", "include", "Luau", "Bytecode.h"), "utf8");
	const compat = path.join(out, "src", "luau", "compat.rs");
	let text = fs.readFileSync(compat, "utf8");
	for (const name of ["LBC_VERSION_MAX", "LBC_TYPE_VERSION_MIN", "LBC_TYPE_VERSION_MAX"]) {
		const value = new RegExp(`^\\s*${name} = (\\d+),`, "m").exec(header)?.[1];
		const constant = new RegExp(`^const ${name}: u8 = \\d+;`, "m");
		if (!value || !constant.test(text))
			throw new Error(`${name} is not in the vendored Bytecode.h or not in mlua-sys's src/luau/compat.rs: see how this mlua-sys detects bytecode`);
		text = text.replace(constant, `const ${name}: u8 = ${value};`);
	}
	fs.writeFileSync(compat, text);
}

// The vendored tree, built in `work`
async function build(work, pin = readPin(), series = readSeries()) {
	const lune = await archive(LUNE);
	run(TAR, ["-xzf", lune, "-C", work], work);
	applyPatchText(fs.readFileSync(path.join(ROOT, "patches", "lune.patch"), "utf8"), path.join(work, "lune-0.10.4"), "lune.patch");
	const luau = path.join(work, "luau0-src");
	await luauTree(pin, luau);
	for (const topic of series)
		applyPatchText(topic.body, luau, `patches/luau/${topic.name}.patch`);
	writeFlagValues(luau);
	const out = path.join(work, "vendor");
	fs.mkdirSync(out, { recursive: true });
	fs.cpSync(luau, path.join(out, "luau0-src"), { recursive: true });
	await mluaSysTree(luau, path.join(out, "mlua-sys"));
	for (const crate of ["lune-roblox", "lune-utils"]) {
		fs.cpSync(path.join(work, "lune-0.10.4", "crates", crate), path.join(out, crate), { recursive: true });
		stripWorkspaceLints(path.join(out, crate, "Cargo.toml"));
		pinMlua(path.join(out, crate, "Cargo.toml"));
		fs.copyFileSync(path.join(work, "lune-0.10.4", "LICENSE.txt"), path.join(out, crate, "LICENSE.txt"));
	}
	return out;
}

function files(directory, base = directory) {
	return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
		const full = path.join(directory, entry.name);
		return entry.isDirectory() ? files(full, base) : [path.relative(base, full).replaceAll("\\", "/")];
	}).sort();
}

// Not compared: vendor/README.md (notes kept across rebuilds) and luau0-src/Cargo.lock, which that crate's own .gitignore
// keeps out of a checkout, so a fresh clone has none
const UNCHECKED = new Set(["README.md", "luau0-src/Cargo.lock"]);

function differences(built) {
	const want = files(built).filter(name => !UNCHECKED.has(name));
	const have = fs.existsSync(VENDOR) ? files(VENDOR).filter(name => !UNCHECKED.has(name)) : [];
	const differing = [...new Set([...want, ...have])].filter(name => {
		const a = path.join(built, name);
		const b = path.join(VENDOR, name);
		return !fs.existsSync(a) || !fs.existsSync(b) || digest(fs.readFileSync(a)) !== digest(fs.readFileSync(b));
	});
	return { count: want.length, differing };
}

async function vendor(check) {
	const work = fs.mkdtempSync(path.join(os.tmpdir(), "ree-vendor-"));
	try {
		const built = await build(work);
		if (check) {
			const { count, differing } = differences(built);
			if (differing.length) {
				console.error(`vendor/ differs from the archives with the patches in ${differing.length} files: ${differing.slice(0, 8).join(", ")}${differing.length > 8 ? ", ..." : ""}`);
				return false;
			}
			console.log(`vendor/ is the pinned archives with the patches (${count} files)`);
			return true;
		}
		const readme = path.join(VENDOR, "README.md");
		const notes = fs.existsSync(readme) ? fs.readFileSync(readme) : null;
		fs.rmSync(VENDOR, { recursive: true, force: true });
		fs.cpSync(built, VENDOR, { recursive: true });
		if (notes) fs.writeFileSync(readme, notes);
		console.log(`vendor/ rebuilt: ${files(VENDOR).length} files`);
		return true;
	} finally {
		fs.rmSync(work, { recursive: true, force: true });
	}
}

// ---- bumping the pinned Luau

async function json(url) {
	return JSON.parse((await download(url)).toString("utf8"));
}

function upstreamTags() {
	const result = capture("git", ["ls-remote", "--tags", `${UPSTREAM}.git`]);
	if (result.status !== 0) throw new Error(`git ls-remote ${UPSTREAM}: ${result.stderr.trim()}`);
	const tags = new Map();
	for (const line of result.stdout.split("\n")) {
		const match = /^([0-9a-f]{40})\trefs\/tags\/(0\.\d+)(\^\{\})?$/.exec(line.trim());
		if (match && (match[3] || !tags.has(match[2]))) tags.set(match[2], match[1]);
	}
	return tags;
}

// The release a live Roblox client runs: the second field of its version (0.746.0.7461267 runs release 746)
async function clientRelease() {
	const { version } = await json("https://clientsettingscdn.roblox.com/v2/client-version/WindowsPlayer");
	const match = /^0\.(\d+)\./.exec(version);
	if (!match) throw new Error(`a client version this tool does not read: ${version}`);
	return { release: `0.${match[1]}`, version };
}

// The commit that synced upstream's release branch NNN, among master's recent commits
async function syncCommit(release) {
	const wanted = `Sync to upstream/release/${releaseNumber(release)}`;
	for (let page = 1; page <= 3; page++) {
		const commits = await json(`https://api.github.com/repos/luau-lang/luau/commits?per_page=100&page=${page}`);
		const found = commits.find(commit => commit.commit.message.split("\n")[0].startsWith(wanted));
		if (found) return found.sha;
		if (commits.length < 100) break;
	}
	throw new Error(`no "${wanted}" commit on luau-lang/luau master`);
}

// { luau, ref, file, url }
async function resolveTarget(spec, release) {
	const tags = upstreamTags();
	const fromTag = tag => ({ luau: tag, ref: tag, file: `luau-${tag}.tar.gz`, url: `${UPSTREAM}/archive/refs/tags/${tag}.tar.gz` });
	const fromCommit = (sha, luau) => ({ luau, ref: sha, file: `luau-${sha.slice(0, 12)}.tar.gz`, url: `${UPSTREAM}/archive/${sha}.tar.gz` });
	if (spec === "latest") {
		const newest = [...tags.keys()].sort((a, b) => releaseNumber(b) - releaseNumber(a))[0];
		return fromTag(newest);
	}
	if (spec === "client") {
		const client = await clientRelease();
		console.error(`vendor: a live client runs ${client.version}: Luau ${client.release}`);
		return tags.has(client.release) ? fromTag(client.release) : fromCommit(await syncCommit(client.release), client.release);
	}
	if (/^0\.\d+$/.test(spec)) {
		if (!tags.has(spec)) throw new Error(`luau-lang/luau has no tag ${spec} (\`client\` finds an untagged release's sync commit)`);
		return fromTag(spec);
	}
	if (/^[0-9a-f]{7,40}$/.test(spec)) {
		const commit = await json(`https://api.github.com/repos/luau-lang/luau/commits/${spec}`);
		const synced = /^Sync to upstream\/release\/(\d+)/.exec(commit.commit.message);
		const luau = release || (synced ? `0.${synced[1]}` : null);
		if (!luau) throw new Error(`${spec} is not a release sync commit: say which release it is with --release 0.NNN`);
		return fromCommit(commit.sha, luau);
	}
	throw new Error(`a target is client, latest, a release tag (0.746) or a commit: ${spec}`);
}

async function targetPin(spec, options, current) {
	const target = await resolveTarget(spec, options.release);
	console.error(`vendor: target Luau ${target.luau} (${target.ref}) from ${target.url}`);
	const bytes = await download(target.url);
	fs.mkdirSync(CACHE, { recursive: true });
	fs.writeFileSync(path.join(CACHE, target.file), bytes);
	let crate = current.crate;
	if (options.crate) {
		const file = `luau0-src-${options.crate}.crate`;
		const url = `https://static.crates.io/crates/luau0-src/${file}`;
		const crateBytes = await download(url);
		fs.writeFileSync(path.join(CACHE, file), crateBytes);
		crate = { file, url, sha256: digest(crateBytes) };
	}
	const crateVersion = /^luau0-src-(.+)\.crate$/.exec(crate.file)?.[1];
	if (!crateVersion) throw new Error(`a crate file name this tool does not read: ${crate.file}`);
	return {
		luau: target.luau,
		source: { ref: target.ref, file: target.file, url: target.url, sha256: digest(bytes) },
		crate,
		version: crateVersion.replace(/\+luau\d+$/, "") + `+luau${releaseNumber(target.luau)}`,
	};
}

function git(work, parameters, options = {}) {
	const result = capture("git", ["-c", "core.autocrlf=false", "-c", "core.safecrlf=false", "-c", "core.quotepath=false", "-c", "user.name=ree-vendor", "-c", "user.email=ree-vendor@localhost", "-c", "core.editor=true", "-c", "sequence.editor=true", ...parameters], work);
	if (result.status !== 0 && !options.allowFailure) throw new Error(`git ${parameters.join(" ")}: ${(result.stderr || result.stdout).trim()}`);
	return result;
}

function commitTree(work, message) {
	git(work, ["add", "-A"]);
	git(work, ["commit", "-q", "--allow-empty", "--no-verify", "-m", message]);
	return git(work, ["rev-parse", "HEAD"]).stdout.trim();
}

// Where a conflicted file's conflicts are: the line of each <<<<<<< marker
function conflicts(work) {
	const names = git(work, ["diff", "--name-only", "--diff-filter=U"]).stdout.split("\n").filter(Boolean);
	return names.map(name => {
		const lines = fs.readFileSync(path.join(work, name), "utf8").split("\n");
		const at = lines.flatMap((line, index) => line.startsWith("<<<<<<<") ? [index + 1] : []);
		return { name, at };
	});
}

// A work repository with the current series as commits on the pinned release (branch `current`), and the target release
// on a branch of its own when there is one; returns the topics' commits, in order
async function prepare(work, current, series, target) {
	fs.rmSync(work, { recursive: true, force: true });
	fs.mkdirSync(work, { recursive: true });
	git(work, ["init", "-q", "-b", "current"]);
	// (also for the git commands a person runs there while resolving a conflict)
	git(work, ["config", "core.autocrlf", "false"]);
	await luauTree(current, work);
	commitTree(work, `Luau ${current.luau}`);
	const commits = [];
	for (const topic of series) {
		applyPatchText(topic.body, work, `patches/luau/${topic.name}.patch`);
		commits.push(commitTree(work, topic.name));
	}
	if (!target)
		return commits;
	git(work, ["checkout", "-q", "--orphan", "target"]);
	git(work, ["rm", "-rq", "--cached", "."]);
	for (const entry of fs.readdirSync(work)) {
		if (entry !== ".git") fs.rmSync(path.join(work, entry), { recursive: true, force: true });
	}
	await luauTree(target, work);
	commitTree(work, `Luau ${target.luau}`);
	return commits;
}

// Cherry-picks topics from `state.next` on, until one conflicts (false) or all are on (true)
function pick(work, state, series) {
	for (; state.next < series.length; state.next++) {
		const topic = series[state.next];
		if (topic.upstream && releaseNumber(topic.upstream) <= releaseNumber(state.target.luau)) {
			console.log(`  ${topic.name}: dropped (upstream has it from ${topic.upstream})`);
			state.dropped.push(topic.name);
			continue;
		}
		// (--keep-redundant-commits: a topic the release already has becomes an empty commit, reported by finish)
		const result = git(work, ["cherry-pick", "--allow-empty", "--keep-redundant-commits", "--no-rerere-autoupdate", state.commits[state.next]], { allowFailure: true });
		if (result.status === 0) {
			console.log(`  ${topic.name}: applied`);
			state.applied.push(topic.name);
			continue;
		}
		const found = conflicts(work);
		if (!found.length) throw new Error(`${topic.name}: cherry-pick failed: ${(result.stderr || result.stdout).trim()}`);
		state.conflicted = { topic: topic.name, files: found };
		return false;
	}
	return true;
}

function saveState(state) {
	fs.writeFileSync(path.join(BUMP, "..", "bump.json"), JSON.stringify(state, null, "\t"));
}

function loadState() {
	const file = path.join(BUMP, "..", "bump.json");
	if (!fs.existsSync(file)) throw new Error("no bump in progress");
	return JSON.parse(fs.readFileSync(file, "utf8"));
}

function reportConflict(state) {
	const { topic, files: found } = state.conflicted;
	console.error(`\n${topic} conflicts with Luau ${state.target.luau}:`);
	for (const { name, at } of found) console.error(`  ${name}: line${at.length === 1 ? "" : "s"} ${at.join(", ")}`);
	console.error(`\nResolve the conflicts in ${BUMP} (the patch's header says what the topic does), \`git add\` the files, then run`);
	console.error("  node tools/vendor.mjs bump --continue");
	console.error("or drop the bump with `node tools/vendor.mjs bump --abort`.");
}

// The finished series: every applied topic's patch rewritten from its commit on the target, dropped backports removed,
// the pin moved, vendor/ rebuilt and checked
async function finish(state, series, options) {
	const log = git(BUMP, ["log", "--reverse", "--format=%H %s", "target"]).stdout.trim().split("\n").slice(1);
	const commitOf = new Map(log.map(line => [line.slice(41), line.slice(0, 40)]));
	for (const topic of series) {
		if (state.dropped.includes(topic.name)) {
			fs.rmSync(topic.file);
			continue;
		}
		const commit = commitOf.get(topic.name);
		if (!commit) throw new Error(`${topic.name}: no commit on the target branch`);
		const body = git(BUMP, ["diff", "--no-color", "--no-ext-diff", "--no-renames", "--src-prefix=a/", "--dst-prefix=b/", "-U3", `${commit}^`, commit]).stdout;
		if (!body.trim()) {
			// (the release makes the same change: nothing of the topic is left)
			console.log(`  ${topic.name}: nothing left of it on Luau ${state.target.luau}: removed`);
			state.applied.splice(state.applied.indexOf(topic.name), 1);
			state.dropped.push(topic.name);
			fs.rmSync(topic.file);
			continue;
		}
		fs.writeFileSync(topic.file, topic.header + body);
	}
	writePin(state.target);
	fs.rmSync(path.join(BUMP, "..", "bump.json"), { force: true });
	if (!(await vendor(false)) || !(await vendor(true)))
		throw new Error("vendor/ could not be rebuilt from the new series");
	fs.rmSync(BUMP, { recursive: true, force: true });
	console.log(`\nLuau ${state.from} -> ${state.target.luau}: ${state.applied.length} topics rebased${state.resolved.length ? ` (${state.resolved.join(", ")} by hand)` : ""}${state.dropped.length ? `, ${state.dropped.length} backports dropped (${state.dropped.join(", ")})` : ""}.`);
	const steps = [
		["node tools/luau-flags.mjs", "the live client's flag values for this release (data/luau-flags.json), then vendor/ again"],
		["cargo build --release && cargo test --release", "the engine's tests and the patched VM's (tests/luau_vm.rs)"],
		["node tools/parity/run.mjs --diff", "behaviour against a real client: it holds or improves"],
		["tools/cost-parity (README)", "prices against a client: refit what the release moved, held-out rows apart"],
		["a host's own checks", "a crate that pins this one (roblox-ui-renderer) moves both pins and runs its goldens"],
	];
	console.log("\nNext:");
	steps.forEach(([command, what], index) => console.log(`  ${index + 1}. ${command.padEnd(48)} ${what}`));
	if (options.verify) {
		for (const [command, parameters] of [["cargo", ["build", "--release"]], ["cargo", ["test", "--release"]], ["node", ["tools/parity/run.mjs"]]]) {
			console.log(`\n$ ${command} ${parameters.join(" ")}`);
			run(command, parameters, ROOT);
		}
	}
}

async function bump(spec, options) {
	if (options.abort) {
		fs.rmSync(path.join(BUMP, "..", "bump.json"), { force: true });
		fs.rmSync(BUMP, { recursive: true, force: true });
		console.log("bump dropped: patches/ and vendor/ are as they were");
		return true;
	}
	const series = readSeries();
	let state;
	if (options.continue) {
		state = loadState();
		const result = git(BUMP, ["cherry-pick", "--continue"], { allowFailure: true });
		if (result.status !== 0) {
			const left = conflicts(BUMP);
			if (left.length) {
				state.conflicted.files = left;
				reportConflict(state);
				return false;
			}
			// (nothing left to commit: the resolution took the target's side whole; finish removes the empty topic)
			git(BUMP, ["commit", "-q", "--allow-empty", "--no-verify", "-m", state.conflicted.topic]);
			git(BUMP, ["cherry-pick", "--quit"], { allowFailure: true });
		}
		console.log(`  ${state.conflicted.topic}: resolved`);
		state.resolved.push(state.conflicted.topic);
		state.applied.push(state.conflicted.topic);
		state.conflicted = null;
		state.next++;
	} else {
		if (fs.existsSync(path.join(BUMP, "..", "bump.json")))
			throw new Error("a bump is in progress: `bump --continue` or `bump --abort`");
		const current = readPin();
		const target = await targetPin(spec, options, current);
		if (target.source.ref === current.source?.ref)
			console.error(`vendor: Luau ${target.luau} is already the pin; rebasing onto it anyway`);
		console.log(`Rebasing patches/luau/ from Luau ${current.luau} onto ${target.luau}:`);
		const commits = await prepare(BUMP, current, series, target);
		state = { from: current.luau, target, commits, next: 0, applied: [], dropped: [], resolved: [], conflicted: null };
	}
	if (!pick(BUMP, state, series)) {
		saveState(state);
		reportConflict(state);
		return false;
	}
	await finish(state, series, options);
	return true;
}

// Whether the series applies to a release, topic by topic; changes nothing
async function attempt(spec, options) {
	const current = readPin();
	const series = readSeries();
	const target = await targetPin(spec, options, current);
	const work = fs.mkdtempSync(path.join(os.tmpdir(), "ree-vendor-try-"));
	try {
		console.log(`Luau ${current.luau} -> ${target.luau} (${target.source.ref}):`);
		const commits = await prepare(work, current, series, target);
		const state = { target, commits, next: 0, applied: [], dropped: [], resolved: [], conflicted: null };
		const failed = [];
		while (!pick(work, state, series)) {
			const { topic, files: found } = state.conflicted;
			console.log(`  ${topic}: CONFLICTS in ${found.map(({ name, at }) => `${name} (line${at.length === 1 ? "" : "s"} ${at.join(", ")})`).join(", ")}`);
			failed.push(topic);
			git(work, ["cherry-pick", "--abort"], { allowFailure: true });
			state.conflicted = null;
			state.next++;
		}
		console.log(failed.length
			? `\n${failed.length} of ${series.length} topics conflict with Luau ${target.luau} (later topics were tried without them); \`bump\` stops at each for a hand merge.`
			: `\nThe series applies to Luau ${target.luau} as it is${state.dropped.length ? `, with ${state.dropped.length} backports dropped` : ""}: \`node tools/vendor.mjs bump ${spec}\` needs no hand merge.`);
		if (options.keep) console.log(`work tree kept: ${work}`);
		return failed.length === 0;
	} finally {
		if (!options.keep) fs.rmSync(work, { recursive: true, force: true });
	}
}

// ---- changing topics in the middle of the series

const EDIT = path.join(CACHE, "edit");

// A topic's patch from its commit; an empty one means nothing is left of it
function topicBody(work, commit) {
	return git(work, ["diff", "--no-color", "--no-ext-diff", "--no-renames", "--src-prefix=a/", "--dst-prefix=b/", "-U3", `${commit}^`, commit]).stdout;
}

async function edit(options) {
	if (!options.finish) {
		if (fs.existsSync(EDIT)) throw new Error(`an edit is in progress in ${EDIT}: \`edit --finish\`, or delete the folder`);
		const pin = readPin();
		await prepare(EDIT, pin, readSeries());
		console.log(`${EDIT} holds Luau ${pin.luau} with one commit per topic (branch current). Change a topic there as a commit of its own:`);
		console.log("  git commit --fixup=:/<topic>        folded into that topic (its later topics are rebased over it)");
		console.log("  git commit -m NN-<topic>             a new topic; its header is written as a stub to fill in");
		console.log("then run `node tools/vendor.mjs edit --finish`.");
		return true;
	}
	if (!fs.existsSync(EDIT)) throw new Error("no edit in progress: `node tools/vendor.mjs edit`");
	if (fs.existsSync(path.join(EDIT, ".git", "rebase-merge")) || fs.existsSync(path.join(EDIT, ".git", "rebase-apply")))
		throw new Error(`a rebase is stopped in ${EDIT}: finish it (git rebase --continue) first`);
	const root = git(EDIT, ["rev-list", "--max-parents=0", "HEAD"]).stdout.trim();
	if (git(EDIT, ["log", "--format=%s", `${root}..HEAD`]).stdout.split("\n").some(subject => /^(fixup|squash|amend)! /.test(subject))) {
		let result = git(EDIT, ["rebase", "-i", "--autosquash", root], { allowFailure: true });
		// (a stop with nothing in conflict is a topic the fixups emptied: keep it empty, and finish removes it)
		for (let stops = 0; result.status !== 0 && !conflicts(EDIT).length && stops < 64; stops++)
			result = git(EDIT, ["-c", "core.editor=true", "rebase", "--continue"], { allowFailure: true });
		if (result.status !== 0) {
			const found = conflicts(EDIT);
			console.error(`folding the fixups conflicts${found.length ? `: ${found.map(({ name, at }) => `${name} (line${at.length === 1 ? "" : "s"} ${at.join(", ")})`).join(", ")}` : ""}`);
			console.error(`resolve them in ${EDIT}, \`git add\`, \`git rebase --continue\`, then run \`node tools/vendor.mjs edit --finish\` again`);
			return false;
		}
	}
	const series = new Map(readSeries().map(topic => [topic.name, topic]));
	const commits = git(EDIT, ["log", "--reverse", "--format=%H %s", `${root}..HEAD`]).stdout.trim().split("\n").filter(Boolean);
	const kept = new Set();
	for (const line of commits) {
		const [commit, name] = [line.slice(0, 40), line.slice(41)];
		if (!/^\d+-[\w.-]+$/.test(name)) throw new Error(`a commit whose subject is not a topic name (NN-<topic>): ${name}`);
		const body = topicBody(EDIT, commit);
		const file = path.join(SERIES, `${name}.patch`);
		if (!body.trim()) {
			console.log(`  ${name}: nothing left of it: removed`);
			continue;
		}
		const header = series.get(name)?.header ?? "# Topic: (what it is)\n#\n# What it does: (describe it)\n#\n# Why upstream lacks it: (say why)\n\n";
		if (!series.has(name)) console.log(`  ${name}: new; write its header`);
		fs.writeFileSync(file, header + body);
		kept.add(name);
	}
	for (const [name, topic] of series) {
		if (!kept.has(name)) {
			fs.rmSync(topic.file);
			console.log(`  ${name}: removed`);
		}
	}
	if (!(await vendor(false)) || !(await vendor(true)))
		throw new Error("vendor/ could not be rebuilt from the edited series");
	fs.rmSync(EDIT, { recursive: true, force: true });
	return true;
}

function parse(argv) {
	const options = {};
	const positional = [];
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		if (arg === "--crate" || arg === "--release") options[arg.slice(2)] = argv[++i];
		else if (arg.startsWith("--")) options[arg.slice(2)] = true;
		else positional.push(arg);
	}
	return { options, positional };
}

const { options, positional } = parse(process.argv.slice(2));
const [command, target] = positional;
try {
	let ok;
	if (command === "bump") {
		if (!target && !options.continue && !options.abort) throw new Error("bump <client|latest|0.NNN|commit> [--crate <luau0-src version>] [--verify], or bump --continue|--abort");
		ok = await bump(target, options);
	} else if (command === "edit") {
		ok = await edit(options);
	} else if (command === "try") {
		if (!target) throw new Error("try <client|latest|0.NNN|commit> [--keep]");
		ok = await attempt(target, options);
	} else if (command === undefined) {
		ok = await vendor(Boolean(options.check));
	} else {
		throw new Error(`unknown command ${command}: (none), --check, bump, edit, try`);
	}
	process.exitCode = ok ? 0 : 1;
} catch (error) {
	console.error(`vendor: ${error.message}`);
	process.exitCode = 2;
}

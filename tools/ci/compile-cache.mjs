// The compile cache of the CI jobs (.github/workflows/ci.yml): sccache between cargo and the compilers.
//
//   node tools/ci/compile-cache.mjs start --tools <folder> --cache <folder>
//   node tools/ci/compile-cache.mjs stop
//
// The dependency cache the jobs restore (Swatinem/rust-cache) keeps what the crates.io crates compile to, but nothing of
// vendor/. Cargo decides by modification times whether a path dependency is fresh, and a checkout makes every file new,
// so each job compiled the Luau VM's C++ and the vendored crates again. sccache decides by content instead: it stores
// what a compile wrote under a hash of the compiler, its arguments, its environment and every file it read, and hands
// that back when the same compile is asked for again. A change in vendor/ changes the hash, so it always compiles.
//
// `start` downloads the pinned sccache release into --tools, checks its SHA-256 and gives the job's later steps this
// environment (GITHUB_ENV). A download that fails is tried again a few times, and a job never builds without the cache:
//
//   RUSTC_WRAPPER       cargo starts rustc through sccache. sccache answers for library crates only. Whatever is linked
//                       (a binary, a test binary, a build script, a procedural macro) is always built, since sccache
//                       cannot see what the linker read
//   CARGO_INCREMENTAL   0, as the toolchain step sets it already. The release profile asks for incremental builds,
//                       and sccache answers for none of those
//   CXX, INCLUDE        the cc crate compiles the C++ of two build scripts: the VM in mlua-sys's, and mimalloc in
//                       libmimalloc-sys's, which is built as C++ with Microsoft's compiler. It puts sccache in front of
//                       the C++ compiler only when the environment names the compiler: the one it finds by itself runs
//                       unwrapped. So the same compiler is named here, Visual Studio's default cl.exe, with the include
//                       paths of Visual Studio's own environment script, which a compiler named this way needs. Both
//                       builds go through it, and their objects are byte for byte what cc's own compiler writes. The
//                       C builds (zstd, blake3) keep the compiler cc finds, unwrapped
//   SCCACHE_DIR, ...    the cache is the folder --cache alone. The workflow restores it before the build and stores it
//                       after a run on main. sccache is never given the workflow's token
//   SCCACHE_DIRECT      false: a C++ compile is always looked up by what the preprocessor made of it (see `start`)
//
// and this output (GITHUB_OUTPUT):
//
//   key                 the name the workflow stores the folder under: a hash of everything the vendored builds are made
//                       from. A run that finds its key stored has nothing new to store
//
// The dependency cache's key takes in the compiler variables, so `start` runs before that cache is restored: what it
// holds was then built with the same variables, and cargo finds it fresh.
//
// `stop` prints what the cache did in this job and stops sccache's server, which holds the folder open until then.
// A job in which no server is running has none to stop, and `stop` says so.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { ROOT, args, command, isMain, main } from "../release/common.mjs";

// The pinned release (https://github.com/mozilla/sccache/releases) and the SHA-256 of its archive
const SCCACHE = {
	url: "https://github.com/mozilla/sccache/releases/download/v0.19.0/sccache-v0.19.0-x86_64-pc-windows-msvc.zip",
	sha256: "28be8c61f6b2c89b9eb2869f6af7f78e6f992e78a8abae0ce855fcc129cbeac1",
};

// What the vendored builds are made from besides the compilers: the vendored sources, the versions and features of the
// crates they depend on, the build settings, and the workflow and this script, which set the build's environment.
// Folders count with every file below them.
const KEY_INPUTS = [
	"vendor", "Cargo.lock", "Cargo.toml", "profile-view/Cargo.toml", "tests/host/Cargo.toml", ".cargo/config.toml",
	".github/workflows/ci.yml", "tools/ci/compile-cache.mjs",
];

// The most the cache folder holds before sccache drops what was used longest ago. Every job waits for the folder's
// download, so it is kept small. One run's whole output, every crates.io crate included, is 144 MB: this holds that
// and, beside it, what the builds before the last changes of vendor/ or the lockfile stored.
const CACHE_SIZE = "400M";

// What sccache says when it is asked to stop a server and none is running (0.19.0, src/commands.rs)
const NO_SERVER = "couldn't connect to server";

const SYSTEM_ROOT = process.env.SystemRoot || "C:\\Windows";

// `bytes`, unless their SHA-256 is not `sha256`
export function checked(bytes, sha256, name) {
	const actual = createHash("sha256").update(bytes).digest("hex");
	if (actual !== sha256) throw new Error(`${name}: checksum mismatch, got ${actual}`);
	return bytes;
}

// The variables in what cmd's `set` prints, one NAME=value per line
export function parseEnvironment(text) {
	const variables = {};
	for (const line of text.split(/\r?\n/)) {
		const at = line.indexOf("=");
		if (at > 0) variables[line.slice(0, at)] = line.slice(at + 1);
	}
	return variables;
}

// Every file below `folder`, or `folder` itself when it is a file, sorted so that the order is the same on every machine
function files(folder) {
	if (!fs.statSync(folder).isDirectory()) return [folder];
	return fs.readdirSync(folder, { recursive: true, withFileTypes: true })
		.filter(entry => entry.isFile())
		.map(entry => path.join(entry.parentPath, entry.name))
		.sort();
}

// The cache key: a hash of `facts` (the compilers' versions) and of the name and bytes of every file in `inputs`
// under `root`. Line endings are left as they are: vendor/ is checked out byte for byte (.gitattributes).
export function cacheKey(root, inputs, facts) {
	const hash = createHash("sha256");
	for (const fact of facts) hash.update(`${fact}\0`);
	for (const input of inputs) {
		for (const file of files(path.join(root, input))) {
			hash.update(`${path.relative(root, file).replaceAll("\\", "/")}\0`);
			hash.update(fs.readFileSync(file));
			hash.update("\0");
		}
	}
	return `compile-cache-${hash.digest("hex").slice(0, 32)}`;
}

// Why a download failed. Of a network error fetch says only "fetch failed" and keeps the reason in `cause`
function reason(error) {
	const cause = error.cause?.message || error.cause?.code;
	return cause ? `${error.message}: ${cause}` : error.message;
}

// The bytes `get` answers with for `url`; fails when the answer is no success or is not complete after `timeout` ms
async function fetched(url, timeout, get) {
	const response = await get(url, { signal: AbortSignal.timeout(timeout) });
	if (!response.ok) throw new Error(`HTTP ${response.status}`);
	return Buffer.from(await response.arrayBuffer());
}

// The bytes at `url`, used only when their SHA-256 is `sha256`. Every job of a run downloads the archive, so one
// answer that fails or stalls must not fail the run: a failed attempt is followed by another, after a pause that
// grows by `wait` ms each time. A checksum that differs is no network error and fails at once.
export async function download({ url, sha256 }, { attempts = 4, timeout = 30_000, wait = 2_000, get = fetch, pause = sleep, log = console.log } = {}) {
	let bytes;
	for (let attempt = 1; !bytes; attempt++) {
		try {
			bytes = await fetched(url, timeout, get);
		} catch (error) {
			if (attempt >= attempts) throw new Error(`${url}: no download in ${attempts} attempts, the last one failed: ${reason(error)}`);
			log(`${url}: attempt ${attempt} of ${attempts} failed: ${reason(error)}`);
			await pause(wait * attempt);
		}
	}
	return checked(bytes, sha256, url);
}

// Downloads the pinned archive and unpacks it into `tools` once its checksum matches; returns sccache's path
async function install(tools) {
	const archive = path.join(tools, path.basename(SCCACHE.url));
	fs.mkdirSync(tools, { recursive: true });
	fs.writeFileSync(archive, await download(SCCACHE));
	// Windows' own tar reads ZIP archives; Git Bash's, which may come first on PATH, does not. The archive's one folder
	// carries the version in its name and is left out, so sccache's path stays the same from release to release: the
	// dependency cache's key takes in that path.
	command(path.join(SYSTEM_ROOT, "System32", "tar.exe"), ["-xf", archive, "-C", tools, "--strip-components", "1"]);
	return path.join(tools, "sccache.exe");
}

// What Visual Studio's environment script sets for 64-bit builds with the newest installed C++ tools. The cc crate
// picks its compiler the same way: the newest Visual Studio, and the tools version its default file names.
function developerEnvironment() {
	const vswhere = path.join(process.env["ProgramFiles(x86)"] || "C:\\Program Files (x86)", "Microsoft Visual Studio", "Installer", "vswhere.exe");
	const studio = command(vswhere, ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"]).trim();
	if (!studio) throw new Error("no Visual Studio with the C++ tools is installed");
	const script = path.join(studio, "VC", "Auxiliary", "Build", "vcvars64.bat");
	// cmd takes the line as it is written here: the script's path in quotes, its banner dropped, then every variable
	const line = `""${script}" >nul && set"`;
	// The script adds its include paths to any that are set already, so it starts without: the answer must be the same
	// whatever ran before, since the caches' keys and cargo's own freshness check take in INCLUDE.
	const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => name.toUpperCase() !== "INCLUDE"));
	return parseEnvironment(command(process.env.ComSpec || path.join(SYSTEM_ROOT, "System32", "cmd.exe"), ["/d", "/s", "/c", line], { windowsVerbatimArguments: true, env }));
}

// Appends NAME=value lines to the file GitHub Actions names in the environment variable `variable`
function publish(variable, values) {
	const file = process.env[variable];
	if (!file) throw new Error(`${variable} is not set: this runs as a step of a GitHub Actions job`);
	fs.appendFileSync(file, Object.entries(values).map(([name, value]) => `${name}=${value}\n`).join(""));
}

async function start({ tools, cache }) {
	if (process.platform !== "win32") throw new Error("the compile cache is set up for the Windows jobs only");
	if (!tools || !cache) throw new Error("start needs --tools <folder> and --cache <folder>");
	const sccache = await install(path.resolve(tools));
	const studio = developerEnvironment();
	const compiler = path.join(studio.VCToolsInstallDir ?? "", "bin", "Hostx64", "x64", "cl.exe");
	if (!studio.INCLUDE || !fs.existsSync(compiler)) throw new Error("Visual Studio's environment names no C++ compiler and include paths");
	publish("GITHUB_ENV", {
		RUSTC_WRAPPER: sccache,
		CARGO_INCREMENTAL: "0",
		CXX: compiler,
		INCLUDE: studio.INCLUDE,
		SCCACHE_DIR: path.resolve(cache),
		SCCACHE_CACHE_SIZE: CACHE_SIZE,
		// sccache looks a C++ compile up by the preprocessor's output, which holds every header the compile read. In
		// direct mode it skips the preprocessor and goes by the headers the same source read last time, so a new
		// header that comes first on the include path goes unseen. 0.19.0 does not use that mode with cl.exe, though
		// its statistics say yes of the setting; this keeps a later release from using it here.
		SCCACHE_DIRECT: "false",
		// the server stays up between the job's cargo commands, until `stop`
		SCCACHE_IDLE_TIMEOUT: "0",
	});
	// the compilers are not files of the checkout, so their versions stand in for them, and sccache's for how it hashes
	const facts = [SCCACHE.sha256, command("rustc", ["-vV"]), studio.VCToolsVersion, studio.WindowsSDKVersion];
	const key = cacheKey(ROOT, KEY_INPUTS, facts);
	publish("GITHUB_OUTPUT", { key });
	console.log(`compile cache: ${key}; C++ compiler ${compiler}; Windows SDK ${studio.WindowsSDKVersion}`);
}

// Stops sccache's server, which prints what the cache did in this job as it goes. sccache starts its server for a
// compile and for nothing else, so none is running when no compile of the job went through it, or when the server did
// not outlive the step that started it. Neither is a failure of the job: nothing holds the folder open, and only the
// statistics are missing.
export function stop({ env = process.env, spawn = spawnSync, log = console.log } = {}) {
	const sccache = env.RUSTC_WRAPPER;
	if (!sccache) throw new Error("RUSTC_WRAPPER is not set: `start` did not run in this job");
	const result = spawn(sccache, ["--stop-server"], { encoding: "utf8", windowsHide: true });
	if (result.error) throw new Error(`${sccache} --stop-server: ${result.error.message}`);
	const output = result.stderr || result.stdout || "";
	if (result.status === 0) log(result.stdout);
	else if (output.includes(NO_SERVER)) log("compile cache: no sccache server is running, so there is none to stop and there are no statistics. No compile of this job went through sccache, or its server did not outlive the step that started it.");
	else throw new Error(`${sccache} --stop-server: exit ${result.status}: ${output}`);
}

if (isMain(import.meta.url)) {
	await main(async () => {
		const [mode, ...rest] = process.argv.slice(2);
		if (mode === "start") await start(args(rest, ["tools", "cache"]));
		else if (mode === "stop" && rest.length === 0) stop();
		else throw new Error("usage: compile-cache.mjs start --tools <folder> --cache <folder> | stop");
	});
}

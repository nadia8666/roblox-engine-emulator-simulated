// Tests of the release scripts: node --test tools/release/test.mjs
// tests/standalone.rs runs the full installed smoke on the built binary.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";
import { ROOT, MANIFESTS, PLATFORMS, args, command, isMain, realPath, sourceText, version } from "./common.mjs";
import { licenses, normalPackages } from "./licenses.mjs";
import { DOCUMENTS, DOCUMENT_MODE, EXECUTABLE_MODE, checkArchive, crc32, readZip, zip } from "./package.mjs";
import { notes } from "./notes.mjs";
import { checkAssetWorker, removeTestFolder, servedRun } from "./smoke.mjs";

// A fake `ree` whose single service has pid 41 and has completed one job. Once the job has run, the service has
// completed a second job ("served"), still shows one ("fallback": the client ran the job itself), or has been replaced
// by another process ("replaced").
function fakeServiceClient(outcome) {
	let jobRan = false;
	return (argv) => {
		if (argv[0] !== "serve") {
			jobRan = true;
			return "job output";
		}
		const pid = jobRan && outcome === "replaced" ? 42 : 41;
		const done = jobRan && outcome !== "fallback" ? 2 : 1;
		return JSON.stringify({ pid, health: { jobs: { done } } });
	};
}

test("served-run checks reject client fallback and a different service PID", async (t) => {
	// every clock read jumps past the 10 second limits, so the polling gives up at once instead of waiting
	let now = 0;
	t.mock.method(Date, "now", () => now += 10_001);
	assert.equal(await servedRun(fakeServiceClient("served"), ["run", "--service"], 41), "job output");
	for (const outcome of ["fallback", "replaced"]) {
		await assert.rejects(servedRun(fakeServiceClient(outcome), ["run", "--service"], 41), /expected service must/);
	}
});

test("served-run checks wait for previous work and delayed completion after the reply", async () => {
	// The job counts that successive `serve --status` calls report: first an earlier job queued, then running, then
	// done, which makes 5; after the reply the new job is still running, then released, and only then counted as the 6th.
	const beforeTheJob = [{ done: 4, queued: 1 }, { done: 4, running: 1 }, { done: 5, running: 0 }];
	const afterTheReply = [{ done: 5, running: 1 }, { done: 5, running: 0 }, { done: 6, running: 0 }];
	let jobRan = false;
	let priorPolls = 0;
	let completionPolls = 0;
	const ree = (argv) => {
		if (argv[0] === "run") {
			assert.equal(priorPolls, beforeTheJob.length, "the previous job must settle before this one starts");
			jobRan = true;
			return "job output";
		}
		const jobs = jobRan ? afterTheReply[completionPolls++] : beforeTheJob[priorPolls++];
		return JSON.stringify({ pid: 41, health: { jobs } });
	};
	assert.equal(await servedRun(ree, ["run", "--service"], 41), "job output");
	assert.equal(completionPolls, afterTheReply.length, "a reply can precede the completed-job counter");
});

test("a new-service check cannot be satisfied by an existing process or client fallback", async (t) => {
	// Every clock read jumps past servedRun's 10 s deadlines, so each wait ends after its first poll.
	let now = 0;
	t.mock.method(Date, "now", () => now += 10_001);
	// "served": a new process 42 completed one job. "old-only": no new process, only the old one 41 counted a job.
	// "fallback": a new process exists but completed nothing, so the client ran the job itself.
	for (const outcome of ["served", "old-only", "fallback"]) {
		let jobRan = false;
		const ree = (argv) => {
			if (!argv.includes("--status")) {
				jobRan = true;
				return "warm output";
			}
			const services = [{ pid: 41, health: { jobs: { done: jobRan ? 8 : 7 } } }];
			if (jobRan && outcome !== "old-only") {
				const newServiceDone = outcome === "served" ? 1 : 0;
				services.push({ pid: 42, health: { jobs: { done: newServiceDone } } });
			}
			return services.map((service) => JSON.stringify(service)).join("\n");
		};
		const job = servedRun(ree, ["serve", "--warm"], undefined, { newService: true });
		if (outcome === "served") assert.equal(await job, "warm output");
		else await assert.rejects(job, /expected service must/);
	}
});

test("the Unix asset-worker smoke runs only when the tested binary supports bundles", (t) => {
	const temp = fs.mkdtempSync(path.join(os.tmpdir(), "ree-release-bundles-"));
	t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
	const env = { REE_TEST_BINARY: "installed-ree" };
	for (const capability of ["unavailable", "available"]) {
		// a fake ree that prints what the capability probe would print, and a recorder in place of the node test run
		const calls = [];
		const ree = (argv) => {
			assert.equal(argv[0], "run");
			assert(fs.readFileSync(argv[2], "utf8").includes("this build downloads no assets"));
			return JSON.stringify({ errors: 0, output: [`bundles capability: ${capability}`] });
		};
		checkAssetWorker(ree, env, temp, (...args) => calls.push(args));
		const workerTestRuns = capability === "available" ? 1 : 0;
		assert.equal(calls.length, workerTestRuns);
		if (workerTestRuns) {
			const [, nodeArguments, options] = calls[0];
			assert(nodeArguments.includes(path.join(ROOT, "tests/asset_worker.mjs")));
			assert.equal(options.env.REE_TEST_BINARY, env.REE_TEST_BINARY);
		}
	}
	// a probe that reports an error fails the smoke instead of skipping the worker tests
	assert.throws(() => checkAssetWorker(() => JSON.stringify({ errors: 1 }), env, temp));
});

test("temporary-folder removal retries an initial busy handle and later Windows removal races", async (t) => {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "ree-release-cleanup-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	const remove = fs.promises.rm;
	const failures = ["EBUSY", "EPERM", "ENOTEMPTY"];
	let attempts = 0;
	t.mock.method(fs.promises, "rm", async (...args) => {
		const code = failures[attempts++];
		if (code) throw Object.assign(new Error(code), { code, path: root });
		return remove(...args);
	});
	assert.equal(await removeTestFolder(root), true);
	assert.equal(attempts, 4);
	assert(!fs.existsSync(root));
});

test("a held temporary folder reports its path without replacing the check result", async (t) => {
	const root = path.join(os.tmpdir(), "ree-release-held");
	const held = path.join(root, "state");
	t.mock.method(fs.promises, "rm", async () => { throw Object.assign(new Error("busy"), { code: "EBUSY", path: held }); });
	// Advance past the retry deadline without making the test wait ten seconds.
	let now = 0;
	t.mock.method(Date, "now", () => now += 11_000);
	const diagnostic = t.mock.method(console, "error", () => {});
	assert.equal(await removeTestFolder(root), false);
	assert(diagnostic.mock.calls[0].arguments[0].includes(held));
	assert(diagnostic.mock.calls[0].arguments[0].includes(`files kept at ${root}`));
});

test("real paths accept Windows drive-relative, drive-root and verbatim paths", { skip: process.platform !== "win32" }, (t) => {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "ree-release-path-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	const driveRoot = path.parse(root).root;
	const drive = driveRoot.slice(0, -1);
	assert.equal(realPath(drive), realPath(path.resolve(drive)));
	assert.equal(realPath(driveRoot), driveRoot);
	assert.equal(realPath(path.toNamespacedPath(driveRoot)), driveRoot);
	assert.equal(realPath(path.toNamespacedPath(root)), realPath(root));
});

test("release scripts run their main step through a linked folder", (t) => {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "ree-release-main-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	// the space in the name is on purpose: a linked checkout can sit in any folder
	const linked = path.join(root, "linked checkout");
	fs.symlinkSync(ROOT, linked, process.platform === "win32" ? "junction" : "dir");

	// this file started as the script, from the real folder, and is asked about through the linked one
	assert.equal(isMain(pathToFileURL(path.join(linked, "tools/release/test.mjs")).href), true);
	// each script started through the link reaches its main step, which prints its usage and fails with no arguments
	for (const script of ["licenses.mjs", "package.mjs", "notes.mjs", "smoke.mjs"]) {
		assert.throws(() => command(process.execPath, [path.join(linked, "tools/release", script)]), /exit 1: usage:/, script);
	}
});

test("main detection compares file URLs when a path cannot be resolved", (t) => {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "ree-release-main-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	const original = process.argv[1];
	t.after(() => { process.argv[1] = original; });
	const missing = path.join(root, "missing.mjs");

	// the started script does not exist: only its own URL is the main module
	process.argv[1] = missing;
	assert.equal(isMain(pathToFileURL(missing).href), true);
	assert.equal(isMain(import.meta.url), false);

	// a module that does not exist is never the started script
	process.argv[1] = original;
	assert.equal(isMain(pathToFileURL(missing).href), false);
	assert.equal(isMain(import.meta.url), true);

	// a process with no script (an interactive session) has no main module
	delete process.argv[1];
	assert.equal(isMain(import.meta.url), false);
});

test("release manifests, lockfile and exact tag must agree", (t) => {
	const current = version();
	assert.equal(command("node", ["tools/release/version.mjs", "--tag", `v${current}`]).trim(), current);
	assert.throws(() => command("node", ["tools/release/version.mjs", "--tag", `v${current}-rc.1`]), /does not match/);

	// a copy of the manifests and lockfile to break
	const copy = fs.mkdtempSync(path.join(os.tmpdir(), "ree-release-version-"));
	t.after(() => fs.rmSync(copy, { recursive: true, force: true }));
	for (const file of [...MANIFESTS, "Cargo.lock"]) {
		fs.mkdirSync(path.dirname(path.join(copy, file)), { recursive: true });
		fs.copyFileSync(path.join(ROOT, file), path.join(copy, file));
	}
	assert.equal(version(copy), current);

	const viewerManifest = path.join(copy, MANIFESTS[1]);
	fs.writeFileSync(viewerManifest, fs.readFileSync(viewerManifest, "utf8").replace(`version = "${current}"`, 'version = "8.0.0"'));
	assert.throws(() => version(copy), /versions must agree/);

	fs.copyFileSync(path.join(ROOT, MANIFESTS[1]), viewerManifest);
	fs.writeFileSync(path.join(copy, "Cargo.lock"), "");
	assert.throws(() => version(copy), /Cargo.lock disagrees/);
});

test("normal dependencies alone are visited, including shared and cyclic edges", () => {
	const ids = ["engine", "normal", "shared", "build", "dev", "viewer"];
	const packages = ids.map((id) => ({ id, name: id === "engine" ? "roblox-engine-emulator" : id, version: "1.0.0" }));
	const dependency = (pkg, kind) => ({ pkg, name: pkg, dep_kinds: [{ kind, target: null }] });
	for (const pkg of packages) pkg.dependencies = ids.map((name) => ({ name, kind: null, target: null, optional: false }));
	const metadata = {
		packages,
		workspace_members: ["engine", "viewer"],
		resolve: {
			nodes: [
				{ id: "engine", deps: [dependency("normal", null), dependency("shared", null), dependency("build", "build"), dependency("dev", "dev")] },
				// normal and shared depend on each other
				{ id: "normal", deps: [dependency("shared", null)] },
				{ id: "shared", deps: [dependency("normal", null)] },
			],
		},
	};
	assert.deepEqual(normalPackages(metadata).map((pkg) => pkg.id).sort(), ["engine", "normal", "shared"]);
});

test("optional normal dependencies need an enabled feature, including aliases and weak forwarding", () => {
	// "required" is not optional; "renamed" is declared under the alias "alias-name"; "targeted" is for another platform
	const names = ["engine", "required", "direct", "implicit", "renamed", "weak", "inactive", "targeted"];
	const declarations = names.slice(1).map((name) => ({ name, kind: null, target: null, optional: name !== "required" }));
	const declaration = (name) => declarations.find((declared) => declared.name === name);
	declaration("renamed").rename = "alias-name";
	declaration("targeted").target = "cfg(windows)";
	const packages = names.map((name) => ({ id: name, name: name === "engine" ? "roblox-engine-emulator" : name, version: "1.0.0", dependencies: [] }));
	const [engine] = packages;
	engine.dependencies = declarations;
	engine.features = {
		// "direct" comes in through "nested", which names "default" again; "weak" is only configured here, not enabled
		default: ["nested", "implicit", "alias-name/fast", "weak?/fast"],
		nested: ["dep:direct", "default"],
		implicit: ["dep:implicit"],
		unused: ["dep:inactive"],
	};
	// the resolve graph has an edge for every declaration, named by its alias with underscores
	const edgeOf = (declared) => ({
		pkg: declared.name,
		name: (declared.rename ?? declared.name).replaceAll("-", "_"),
		dep_kinds: [{ kind: null, target: null }],
	});
	const metadata = {
		packages,
		workspace_members: ["engine"],
		resolve: {
			nodes: packages.map((pkg) => ({
				id: pkg.id,
				features: pkg.id === "engine" ? ["default"] : [],
				deps: pkg.id === "engine" ? declarations.map(edgeOf) : [],
			})),
		},
	};
	const idsOf = () => normalPackages(metadata).map((pkg) => pkg.id).sort();
	assert.deepEqual(idsOf(), ["direct", "engine", "implicit", "renamed", "required"]);
	metadata.resolve.nodes[0].features.push("dep:weak");
	assert(idsOf().includes("weak"), "weak forwarding configures an independently enabled dependency");
});

test("license output sorts packages and files by code units without locale collation", (t) => {
	const root = fs.mkdtempSync(path.join(os.tmpdir(), "ree-license-order-"));
	t.after(() => fs.rmSync(root, { recursive: true, force: true }));
	const names = ["roblox-engine-emulator", "a-crate", "Z-crate", "libmimalloc-sys", "zstd-sys"];
	const packages = names.map((name) => ({ id: name, name, version: "1.0.0", license: "MIT", manifest_path: path.join(root, name, "Cargo.toml"), dependencies: [] }));
	// every package has two license files whose names sort differently by code unit than by locale
	for (const pkg of packages) {
		fs.mkdirSync(path.dirname(pkg.manifest_path), { recursive: true });
		for (const file of ["LICENSE-z", "LICENSE-A"]) fs.writeFileSync(path.join(path.dirname(pkg.manifest_path), file), file);
	}
	// the texts the output adds for sources that are not crates
	const embeddedFiles = [
		"vendor/luau0-src/luau/LICENSE.txt",
		"vendor/luau0-src/luau/lua_LICENSE.txt",
		"vendor/luau0-src/LICENSE",
		"THIRD_PARTY_NOTICES.md",
		"libmimalloc-sys/c_src/mimalloc/v2/LICENSE",
		"zstd-sys/zstd/LICENSE",
	];
	for (const file of embeddedFiles) {
		fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
		fs.writeFileSync(path.join(root, file), "license text");
	}
	const dependencies = names.slice(1);
	packages[0].dependencies = dependencies.map((name) => ({ name, kind: null, target: null, optional: false }));
	const metadata = {
		packages,
		workspace_root: root,
		workspace_members: [names[0]],
		resolve: {
			nodes: names.map((name) => ({
				id: name,
				deps: name === names[0] ? dependencies.map((pkg) => ({ pkg, name: pkg, dep_kinds: [{ kind: null, target: null }] })) : [],
			})),
		},
	};
	const output = licenses(metadata, root);
	assert(output.indexOf("Z-crate 1.0.0") < output.indexOf("a-crate 1.0.0"));
	assert(output.indexOf("--- LICENSE-A ---") < output.indexOf("--- LICENSE-z ---"));
	// the same output without localeCompare, so no step depends on the locale
	const original = String.prototype.localeCompare;
	try {
		String.prototype.localeCompare = () => { throw new Error("locale collation must not affect releases"); };
		assert.equal(licenses(metadata, root), output);
	} finally {
		String.prototype.localeCompare = original;
	}
});

test("each archive is deterministic, flat and has one executable first with Unix 0755", () => {
	assert.equal(crc32(Buffer.from("123456789")), 0xcbf43926);
	for (const platform of Object.values(PLATFORMS)) {
		const entries = [platform.exe, ...DOCUMENTS].map((name, index) => ({
			name,
			bytes: Buffer.from(`entry ${index}\n`),
			mode: index === 0 ? EXECUTABLE_MODE : DOCUMENT_MODE,
		}));
		const archive = zip(entries);
		assert.deepEqual(zip(entries), archive);
		assert.deepEqual(checkArchive(archive, platform.exe).map(({ name, bytes, mode }) => ({ name, bytes, mode })), entries);

		// the binary must come first, and be the only executable
		assert.throws(() => checkArchive(zip([...entries].reverse()), platform.exe), /layout/);
		assert.throws(() => checkArchive(zip(entries.map((entry) => ({ ...entry, mode: DOCUMENT_MODE }))), platform.exe), /executable modes/);

		// a changed byte in the first entry's name fails the zip's own checks
		const broken = Buffer.from(archive);
		broken[broken.indexOf(Buffer.from(platform.exe)) + platform.exe.length] ^= 1;
		assert.throws(() => readZip(broken));
	}
	assert.throws(() => zip([{ name: "folder/file", bytes: Buffer.alloc(0), mode: DOCUMENT_MODE }]), /flat/);
});

test("notes use the previous reachable release and keep shell-looking subjects as text", () => {
	const sha = "a".repeat(40);
	for (const previous of [null, "v0.0.9"]) {
		const gitCalls = [];
		const subject = 'Support spaces $(echo data) `literal` "quotes"';
		const fakeGit = (program, argv) => {
			assert.equal(program, "git");
			gitCalls.push(argv);
			if (argv[0] === "rev-parse") return sha;
			if (argv[0] === "tag") return ["v0.1.0", previous].filter(Boolean).join("\n");
			return subject + "\n";
		};
		const output = notes("0.1.0", fakeGit);
		assert(output.includes(`- ${subject}\n`));

		// the log covers the commits since the previous release, or all of them for the first
		const logRange = gitCalls[2].at(-1);
		assert.equal(logRange, previous ? "v0.0.9..v0.1.0" : "v0.1.0");
		for (const text of ["Installing", "Verifying", "SHA256SUMS", "gh attestation verify", "Corresponding Source", sha, "Rust 1.90.0"]) {
			assert(output.includes(text), text);
		}
	}
	assert.throws(() => sourceText("0.1.0", "short"), /full SHA/);
});

test("release workflow retains its publication boundary and pins", () => {
	const workflow = fs.readFileSync(path.join(ROOT, ".github/workflows/release.yml"), "utf8");
	const lines = workflow.split("\n");

	// the five jobs run on the public repository only
	assert.equal(lines.filter((line) => /^    if: .*repository.private/.test(line)).length, 5);

	// the publish job runs no repository code, and nothing is cached
	const publish = workflow.split(/^  publish:/m)[1];
	assert(!publish.includes("actions/checkout"));
	assert(!workflow.includes("rust-cache"));

	// the mimalloc define is spelled -D, in the workflow and in the rebuild instructions: Git Bash turns a /D value
	// in the environment into a path, and the define never reaches the compiler
	assert.match(workflow, /CXXFLAGS_x86_64_pc_windows_msvc:.*'-DMI_WIN_INIT_USE_FLS=1'/);
	assert(sourceText("0.1.0", "a".repeat(40)).includes('CXXFLAGS_x86_64_pc_windows_msvc="-DMI_WIN_INIT_USE_FLS=1"'));

	// the built Windows executable is started between the build and the packaging
	const buildStep = workflow.indexOf("run: cargo build --release --locked --bin ree");
	const startupStep = workflow.indexOf('run: \'"target/$TARGET/release/$BINARY" --version\'');
	const packageStep = workflow.indexOf("- name: Licenses and package");
	assert(startupStep > buildStep);
	assert(startupStep < packageStep, "startup must be checked before packaging");

	// it publishes the existing tag, never creating one
	assert(workflow.includes("--verify-tag"));
	assert(!workflow.includes('--target "$TAG"'));

	// every action is pinned to a full commit SHA
	for (const line of lines.filter((line) => line.includes("uses:"))) assert.match(line, /@[0-9a-f]{40}\b/);

	// contexts enter scripts through env, never inside a `run:` script
	let inRun = false;
	for (const line of lines) {
		if (/^        run:/.test(line)) inRun = true;
		else if (line.trim() && !/^          /.test(line)) inRun = false;
		if (inRun) assert(!line.includes("${{"), "contexts must enter scripts through env");
	}

	// the command line reads values with spaces, and rejects a name without its value
	assert.deepEqual(args(["--out", "folder with spaces"], ["out"]), { out: "folder with spaces" });
	assert.throws(() => args(["--out"], ["out"]), /incomplete/);
});

// THIRD_PARTY_LICENSES.txt for one target: the license texts of every crate compiled into the binary, plus the
// sources that are not crates.
//
//   node tools/release/licenses.mjs --target <triple> --out <file> [--metadata <json file>]
//
// It reads `cargo metadata --locked --format-version 1 --filter-platform <triple>` (or the file given with
// --metadata, which must have been made that way) and needs the dependencies downloaded but no build. Run without
// a build, it is also the repository's check that no crate lacks its license text.
import fs from "node:fs";
import path from "node:path";
import { ROOT, PLATFORMS, args, command, isMain, main } from "./common.mjs";

const RULE = "=".repeat(72);
const LICENSE_FILE = /^(LICENSE|LICENCE|COPYING|NOTICE|UNLICENSE|COPYRIGHT)/i;

// Sources compiled in that are not crates, as [name, file in this repository]. THIRD_PARTY_NOTICES.md covers the
// UI Labs stand-in.
const EMBEDDED_FILES = [
	["Luau VM", "vendor/luau0-src/luau/LICENSE.txt"],
	["Lua code in Luau", "vendor/luau0-src/luau/lua_LICENSE.txt"],
	["Luau build crate", "vendor/luau0-src/LICENSE"],
	["UI Labs stand-in", "THIRD_PARTY_NOTICES.md"],
];

// C sources inside a crate's package, as [crate, folder in it], whose license files are included too
const EMBEDDED_C_SOURCES = [
	["libmimalloc-sys", "c_src/mimalloc/v2"],
	["zstd-sys", "zstd"],
];

// The packages without a license file in this graph declare these licenses. For each, how to tell its standard text
// from another license file. A license with no matcher here fails the run for review, rather than lacking its text.
const STANDARD_TEXTS = {
	MIT: (text) => /Permission is hereby granted, free of charge/.test(text) && /THE SOFTWARE IS PROVIDED/.test(text),
	"Apache-2.0": (text) => /Apache License/.test(text) && /Version 2\.0, January 2004/.test(text) && /END OF TERMS AND CONDITIONS/.test(text),
	"GPL-2.0-or-later": (text) => /GNU GENERAL PUBLIC LICENSE/.test(text) && /Version 2, June 1991/.test(text) && /END OF TERMS AND CONDITIONS/.test(text),
};

// Compares by code units, which gives the same order on every runner. localeCompare follows the machine's locale.
function byName(a, b) {
	if (a < b) return -1;
	if (a > b) return 1;
	return 0;
}
const forwardSlashes = (file) => file.replaceAll("\\", "/");

// The license and notice files in a package's root, and every file in its LICENSES folder, in name order.
// With `everyFile` all files count, which is how the contents of a LICENSES folder are read.
function licenseFiles(directory, everyFile = false) {
	if (!fs.existsSync(directory)) throw new Error(`missing license directory: ${directory}`);
	const files = [];
	const entries = fs.readdirSync(directory, { withFileTypes: true }).sort((a, b) => byName(a.name, b.name));
	for (const entry of entries) {
		const file = path.join(directory, entry.name);
		if (entry.isDirectory() && (everyFile || entry.name.toUpperCase() === "LICENSES")) files.push(...licenseFiles(file, true));
		else if (entry.isFile() && (everyFile || LICENSE_FILE.test(entry.name))) files.push(file);
	}
	return files;
}

// The names of the optional dependencies that this package's resolved features turn on, as the manifest spells them
// (the alias of a renamed one). Features can name each other, so they are followed to the end. A feature is one of:
//   "dep:name"      turns the dependency on
//   "name/feature"  turns the dependency on and sets its feature
//   "name?/feature" sets the dependency's feature only if something else turned it on (weak forwarding)
//   "other"         another feature of this package, followed in turn
function activeDependencies(pkg, node) {
	const active = new Set();
	const seen = new Set();
	function follow(feature) {
		if (seen.has(feature)) return;
		seen.add(feature);
		if (feature.startsWith("dep:")) {
			active.add(feature.slice("dep:".length));
		} else if (feature.includes("/")) {
			const [dependency] = feature.split("/");
			if (!dependency.endsWith("?")) active.add(dependency);
		} else {
			for (const member of pkg.features?.[feature] ?? []) follow(member);
		}
	}
	for (const feature of node.features ?? []) follow(feature);
	return active;
}

// The normal declarations in the package's manifest behind one edge of the resolve graph. The graph names a renamed
// dependency by its alias with hyphens as underscores, and any other by its package.
function declarationsOfEdge(pkg, edge, edgeKind, packages) {
	const dependencyName = packages.get(edge.pkg).name;
	return (pkg.dependencies ?? []).filter((declared) => {
		if (declared.kind !== null || declared.target !== edgeKind.target) return false;
		if (declared.rename) return declared.rename.replaceAll("-", "_") === edge.name;
		return declared.name === dependencyName;
	});
}

// Whether the edge from `pkg` to a dependency is compiled in: a normal edge whose declaration is not optional, or is
// optional and turned on by a feature.
function isCompiledEdge(pkg, edge, active, packages) {
	return edge.dep_kinds.some((edgeKind) => {
		if (edgeKind.kind !== null) return false;
		const declarations = declarationsOfEdge(pkg, edge, edgeKind, packages);
		return declarations.some((declared) => !declared.optional || active.has(declared.rename ?? declared.name));
	});
}

// The packages compiled into the engine: enabled normal dependencies (not dev or build dependencies), each once,
// sorted by name and version. The resolve graph is already filtered for the build target, so a dependency of another
// platform is not in it.
export function normalPackages(metadata) {
	const engine = metadata.packages.find((pkg) => pkg.name === "roblox-engine-emulator" && metadata.workspace_members.includes(pkg.id));
	if (!engine) throw new Error("metadata has no engine package");
	const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
	const packages = new Map(metadata.packages.map((pkg) => [pkg.id, pkg]));
	const visited = new Set();
	function visit(id) {
		if (visited.has(id)) return;
		visited.add(id);
		const node = nodes.get(id);
		if (!node) throw new Error(`missing resolve node: ${id}`);
		const pkg = packages.get(id);
		const active = activeDependencies(pkg, node);
		for (const dependency of node.deps) {
			if (isCompiledEdge(pkg, dependency, active, packages)) visit(dependency.pkg);
		}
	}
	visit(engine.id);
	return metadata.packages
		.filter((pkg) => visited.has(pkg.id))
		.sort((a, b) => byName(a.name, b.name) || byName(a.version, b.version));
}

// Where a package's files are on this machine. A crate of this workspace is read from `root`, which can differ
// from the folder cargo ran in.
function packageRoot(metadata, pkg, root) {
	const directory = path.dirname(pkg.manifest_path);
	const relative = path.relative(metadata.workspace_root, directory);
	const inWorkspace = relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
	return inWorkspace ? path.join(root, relative) : directory;
}

// Where a package's source comes from, in words
function sourceOf(metadata, pkg) {
	if (pkg.source?.startsWith("registry+")) return `crates.io (${pkg.source})`;
	if (pkg.source?.startsWith("git+")) return pkg.source;
	const relative = forwardSlashes(path.relative(metadata.workspace_root, path.dirname(pkg.manifest_path)));
	if (relative.startsWith("..") || path.isAbsolute(relative)) throw new Error(`path dependency outside repository: ${pkg.name}`);
	return `path in this repository: ${relative || "."}`;
}

// The license names in an SPDX expression: "MIT OR Apache-2.0" gives MIT and Apache-2.0
function licenseIds(expression) {
	return expression.split(/\s+|[()/]/).filter((id) => id && !["OR", "AND"].includes(id));
}

// The text of a file under a heading of its own
function fileSection(heading, file, content) {
	return [RULE, heading, `--- ${file} ---`, content.trimEnd(), ""];
}

export function licenses(metadata, root = ROOT) {
	const text = [
		"Third-party licenses for ree",
		"Generated from the target-filtered, locked default-feature normal dependency graph.",
		"",
	];
	const appendix = new Set(); // licenses whose standard text is added at the end
	const available = []; // the license files read so far, which the appendix takes its texts from

	const packages = normalPackages(metadata);
	for (const pkg of packages) {
		const directory = packageRoot(metadata, pkg, root);
		const files = licenseFiles(directory);
		text.push(RULE, `${pkg.name} ${pkg.version}`, `Source: ${sourceOf(metadata, pkg)}`, `Declared license: ${pkg.license || "not declared"}`, "");
		if (!pkg.license) throw new Error(`license not declared: ${pkg.name}`);
		if (files.length === 0) {
			text.push("This package ships no license file; the declared license text is in the appendix.", "");
			for (const id of licenseIds(pkg.license)) {
				if (!STANDARD_TEXTS[id]) throw new Error(`no standard text matcher for ${id} (${pkg.name})`);
				appendix.add(id);
			}
		}
		for (const file of files) {
			const content = fs.readFileSync(file, "utf8");
			text.push(`--- ${forwardSlashes(path.relative(directory, file))} ---`, content.trimEnd(), "");
			available.push({ name: `${pkg.name} ${pkg.version}/${path.basename(file)}`, text: content });
		}
	}

	// the standard text of each license is taken from another package that ships it, so nothing is fetched
	for (const id of [...appendix].sort()) {
		const source = available.find((file) => STANDARD_TEXTS[id](file.text));
		if (!source) throw new Error(`no local package provides the standard text of ${id}`);
		text.push(RULE, `Standard license appendix: ${id}`, `Text from ${source.name}`, "", source.text.trimEnd(), "");
	}

	for (const [name, file] of EMBEDDED_FILES) {
		text.push(...fileSection(`Embedded source: ${name}`, file, fs.readFileSync(path.join(root, file), "utf8")));
	}
	for (const [crate, folder] of EMBEDDED_C_SOURCES) {
		const pkg = packages.find((pkg) => pkg.name === crate);
		if (!pkg) throw new Error(`missing embedded source package: ${crate}`);
		const directory = packageRoot(metadata, pkg, root);
		for (const file of licenseFiles(path.join(directory, folder))) {
			text.push(...fileSection(`Embedded source: ${crate}`, forwardSlashes(path.relative(directory, file)), fs.readFileSync(file, "utf8")));
		}
	}
	return text.join("\n");
}

if (isMain(import.meta.url)) main(() => {
	const options = args(process.argv.slice(2), ["target", "out", "metadata"]);
	const knownTarget = Object.values(PLATFORMS).some((platform) => platform.target === options.target);
	if (!knownTarget || !options.out) throw new Error("usage: licenses.mjs --target <triple> --out <file> [--metadata <json file>]");
	const metadata = options.metadata
		? fs.readFileSync(options.metadata, "utf8")
		: command("cargo", ["metadata", "--locked", "--format-version", "1", "--filter-platform", options.target]);
	const output = licenses(JSON.parse(metadata));
	fs.mkdirSync(path.dirname(options.out), { recursive: true });
	fs.writeFileSync(options.out, output);
	console.log(options.out);
});

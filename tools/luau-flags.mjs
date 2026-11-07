// The Luau flag values a live Roblox client runs with, for the release vendor/ holds: data/luau-flags.json, which
// tools/vendor.mjs turns into the VM's ClientFlagValues.h (patches/luau/14-client-flags.patch applies them when a VM
// starts or a chunk compiles). Roblox ships Luau changes behind these flags, so a VM with upstream's defaults runs
// another Luau than the client does.
//
//   node tools/luau-flags.mjs            fetch the client's settings, write data/luau-flags.json, rebuild vendor/
//   node tools/luau-flags.mjs --dry-run  only print what would change
//   node tools/luau-flags.mjs --studio   also list the flags Studio serves differently (the engine a roblox-sim session runs)
//
// Only the flags the vendored Luau defines are kept (LUAU_FASTFLAGVARIABLE and its kin in vendor/luau0-src/luau), with
// the prefix Roblox serves them under: FFlag / FInt for static flags, DFFlag / DFInt for dynamic ones. The settings are
// Roblox's public client settings (clientsettingscdn.roblox.com), read only.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LUAU = path.join(ROOT, "vendor", "luau0-src", "luau");
const OUT = path.join(ROOT, "data", "luau-flags.json");
const PIN = path.join(ROOT, "patches", "luau", "upstream.json");
const SETTINGS = "https://clientsettingscdn.roblox.com/v2/settings/application/";
const VERSION = "https://clientsettingscdn.roblox.com/v2/client-version/";
const USER_AGENT = "ree-luau-flags (+https://github.com/Anime-Vanguards/roblox-engine-emulator)";
// the libraries luau0-src builds (vendor.mjs's crate); a flag defined anywhere else never reaches the VM
const LIBRARIES = ["Ast", "Bytecode", "CodeGen", "Common", "Compiler", "Config", "Inliner", "Require", "VM"];

const options = new Set(process.argv.slice(2));

async function json(url) {
	const response = await fetch(url, { headers: { "User-Agent": USER_AGENT } });
	if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
	return response.json();
}

function sources(directory) {
	if (!fs.existsSync(directory)) return [];
	return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
		const full = path.join(directory, entry.name);
		return entry.isDirectory() ? sources(full) : /\.(cpp|h)$/.test(entry.name) ? [full] : [];
	});
}

// The flags the vendored Luau defines: Roblox's name for each (FFlagLuauX) -> { int, def }
function definitions() {
	const found = new Map();
	const pattern = /^LUAU_(DYNAMIC_)?FAST(FLAG|INT)VARIABLE\((\w+)(?:,\s*([^)]+))?\)/gm;
	for (const library of LIBRARIES) {
		for (const file of sources(path.join(LUAU, library))) {
			for (const match of fs.readFileSync(file, "utf8").matchAll(pattern)) {
				const [, dynamic, kind, name, def] = match;
				if (name === "flag") continue; // the macros' own definitions in Common.h
				const int = kind === "INT";
				// an int's default is a constant expression (4 * 1024 * 1024, 1'048'576, 0xFFFF): numbers and arithmetic only
				const expression = int ? def.replaceAll("'", "").trim() : null;
				if (int && !/^[0-9a-fA-FxX*+\-() ]+$/.test(expression)) throw new Error(`${file}: a default this tool does not read: ${def}`);
				const value = int ? Number(Function(`"use strict"; return (${expression});`)()) : (def ?? "false").trim() === "true";
				found.set(`${dynamic ? "D" : ""}F${int ? "Int" : "Flag"}${name}`, { int, def: value });
			}
		}
	}
	return found;
}

const parse = (raw, int) => int ? Number(raw) : String(raw).toLowerCase() === "true";

async function main() {
	const defined = definitions();
	if (!defined.size) throw new Error(`no Luau flags found in ${LUAU}: run node tools/vendor.mjs first`);
	const [{ version }, client] = await Promise.all([json(`${VERSION}WindowsPlayer`), json(`${SETTINGS}PCDesktopClient`)]);
	const served = client.applicationSettings ?? client;
	const flags = {};
	const misnamed = [];
	for (const [key, { int }] of [...defined].sort(([a], [b]) => a.localeCompare(b))) {
		if (key in served) flags[key] = parse(served[key], int);
		else {
			// served under the other kind's prefix: the engine does not apply it either
			const other = key.startsWith("D") ? key.slice(1) : `D${key}`;
			if (other in served) misnamed.push(`${other} (Luau defines ${key})`);
		}
	}
	const pin = fs.existsSync(PIN) ? JSON.parse(fs.readFileSync(PIN, "utf8")).luau : null;
	const data = { client: version, source: `${SETTINGS}PCDesktopClient`, fetched: new Date().toISOString().slice(0, 10), luau: pin, flags };

	const differing = Object.entries(flags).filter(([key, value]) => value !== defined.get(key).def);
	console.log(`Luau ${pin}: ${defined.size} flags defined, ${Object.keys(flags).length} served by the client (${version}), ${differing.length} not at upstream's default:`);
	for (const [key, value] of differing) console.log(`  ${key} = ${value} (default ${defined.get(key).def})`);
	if (misnamed.length) console.log(`served under the wrong kind, so applied by neither: ${misnamed.join(", ")}`);

	if (options.has("--studio")) {
		const studio = (await json(`${SETTINGS}PCStudioApp`)).applicationSettings;
		const other = [...defined].filter(([key, { int }]) => (key in studio ? parse(studio[key], int) : defined.get(key).def) !== (key in flags ? flags[key] : defined.get(key).def));
		console.log(`Studio (PCStudioApp) differs on ${other.length}:`);
		for (const [key, { int }] of other) console.log(`  ${key}: Studio ${key in studio ? parse(studio[key], int) : "(default)"}, client ${key in flags ? flags[key] : "(default)"}`);
	}

	const previous = fs.existsSync(OUT) ? JSON.parse(fs.readFileSync(OUT, "utf8")) : null;
	const changed = Object.keys({ ...flags, ...previous?.flags }).filter(key => flags[key] !== previous?.flags?.[key]);
	console.log(previous ? `${changed.length} flags changed since ${previous.fetched} (${previous.client})${changed.length ? `: ${changed.join(", ")}` : ""}` : "no data/luau-flags.json yet");
	if (options.has("--dry-run")) return;
	fs.writeFileSync(OUT, JSON.stringify(data, null, "\t") + "\n");
	console.log(`wrote ${path.relative(ROOT, OUT)}; rebuilding vendor/`);
	const result = spawnSync(process.execPath, [path.join(ROOT, "tools", "vendor.mjs")], { stdio: "inherit", windowsHide: true });
	process.exitCode = result.status ?? 1;
}

main().catch(error => {
	console.error(`luau-flags: ${error.message}`);
	process.exitCode = 2;
});

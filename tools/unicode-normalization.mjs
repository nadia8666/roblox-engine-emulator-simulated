// The canonical decomposition and composition data utf8.nfcnormalize and utf8.nfdnormalize read (src/lua/normalize.rs):
// data/unicode-normalization.rs, generated from the Unicode Character Database files of the version a live client's
// functions match (docs/engine.md "utf8.nfcnormalize and utf8.nfdnormalize").
//
//   node tools/unicode-normalization.mjs          download the pinned files (SHA-256 checked), write the table
//   node tools/unicode-normalization.mjs --check  the committed table is exactly what the pinned files make (fails otherwise)
//
// A client's data is Unicode 7.0 to 12.1 (their canonical data is the same): it decomposes the 2,060 code points those
// versions give a canonical decomposition and composes their 940 primary composites, and none of Unicode 13.0's or
// later (tools/parity/probes/utf8_normalize.luau: U+11938, Unicode 13.0, and Unicode 16.0's pairs stay as they are).
// The files are the Unicode Consortium's, under the Unicode License V3 (licenses/Unicode-3.0.txt).
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const OUT = path.join(ROOT, "data", "unicode-normalization.rs");
const VERSION = "12.1.0";
const CACHE = path.join(ROOT, ".cache", "unicode", VERSION);
const USER_AGENT = "ree-unicode-normalization (+https://github.com/Anime-Vanguards/roblox-engine-emulator)";
const FILES = [
	{ name: "UnicodeData.txt", sha256: "93ab1acd8fd9d450463b50ae77eab151a7cda48f98b25b56baed8070f80fc936" },
	{ name: "CompositionExclusions.txt", sha256: "abc8394c5bde62453118b00c1c5842160a04d7fffb2e829ee5426b846596d081" },
];

const digest = bytes => createHash("sha256").update(bytes).digest("hex");

// A pinned file in the cache, downloaded when missing (and checked against its SHA-256 either way)
async function pinned({ name, sha256 }) {
	fs.mkdirSync(CACHE, { recursive: true });
	const target = path.join(CACHE, name);
	if (!fs.existsSync(target) || digest(fs.readFileSync(target)) !== sha256) {
		const url = `https://www.unicode.org/Public/${VERSION}/ucd/${name}`;
		console.error(`unicode-normalization: downloading ${url}`);
		const response = await fetch(url, { headers: { "User-Agent": USER_AGENT } });
		if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
		const bytes = Buffer.from(await response.arrayBuffer());
		if (digest(bytes) !== sha256) throw new Error(`${name}: checksum mismatch`);
		fs.writeFileSync(target, bytes);
	}
	return fs.readFileSync(target, "utf8");
}

const hex = (value, width = 4) => value.toString(16).toUpperCase().padStart(width, "0");

function table(unicodeData, exclusionsText) {
	// UnicodeData.txt: field 0 the code point, 3 the canonical combining class, 5 the decomposition (a <tag> first for a
	// compatibility one, which canonical normalization leaves alone)
	const decompositions = new Map();
	const combining = new Map();
	for (const line of unicodeData.split("\n")) {
		if (!line.trim()) continue;
		const fields = line.split(";");
		const code = parseInt(fields[0], 16);
		if (fields[3] !== "0") combining.set(code, Number(fields[3]));
		if (fields[5] && !fields[5].startsWith("<")) decompositions.set(code, fields[5].split(" ").map(part => parseInt(part, 16)));
	}
	const excluded = new Set();
	for (const line of exclusionsText.split("\n")) {
		const match = /^([0-9A-F]{4,6})(?:\.\.([0-9A-F]{4,6}))?\s*#/.exec(line);
		if (!match) continue;
		const last = parseInt(match[2] ?? match[1], 16);
		for (let code = parseInt(match[1], 16); code <= last; code++) excluded.add(code);
	}
	const codes = [...decompositions.keys()].sort((a, b) => a - b);
	for (const code of codes) {
		const parts = decompositions.get(code);
		if (parts.length > 2 || parts.some(part => part === 0 || part >= 1 << 21)) throw new Error(`U+${hex(code)}: unexpected decomposition`);
	}
	// a primary composite: a two-code-point decomposition that is not a full composition exclusion (listed in
	// CompositionExclusions.txt, a non-starter itself, or a decomposition that starts with a non-starter)
	const composites = codes
		.map((code, index) => ({ code, index, parts: decompositions.get(code) }))
		.filter(({ code, parts }) => parts.length === 2 && !excluded.has(code) && !combining.has(code) && !combining.has(parts[0]))
		.sort((a, b) => a.parts[0] - b.parts[0] || a.parts[1] - b.parts[1]);
	return { codes, decompositions, composites };
}

function render({ codes, decompositions, composites }) {
	const packed = codes.map(code => {
		const [first, second = 0] = decompositions.get(code);
		return (BigInt(code) << 42n) | (BigInt(first) << 21n) | BigInt(second);
	});
	const rows = (items, perRow, format) => {
		const lines = [];
		for (let start = 0; start < items.length; start += perRow) lines.push(`    ${items.slice(start, start + perRow).map(format).join(", ")},`);
		return lines.join("\n");
	};
	const secondMin = Math.min(...composites.map(({ parts }) => parts[1]));
	return `// Generated by tools/unicode-normalization.mjs from the Unicode Character Database ${VERSION} (UnicodeData.txt and
// CompositionExclusions.txt, Unicode License V3: licenses/Unicode-3.0.txt). Do not edit; run the tool.
//
// ${codes.length} canonical decompositions and ${composites.length} primary composites; Hangul syllables are composed and decomposed by
// arithmetic (src/lua/normalize.rs) and are not listed.

/// The first and last code point with a canonical decomposition.
pub const DECOMPOSABLE: (u32, u32) = (0x${hex(codes[0])}, 0x${hex(codes.at(-1))});

/// The lowest code point that composes with a code point before it (primary composites only; a Hangul vowel or
/// trailing consonant is higher).
pub const COMPOSING_MIN: u32 = 0x${hex(secondMin)};

/// Each canonical decomposition, one level: \`(code point << 42) | (first << 21) | second\`, sorted by code point. A
/// singleton (a decomposition of one code point) has \`second\` 0.
pub static DECOMPOSITIONS: [u64; ${packed.length}] = [
${rows(packed, 6, value => `0x${value.toString(16).toUpperCase().padStart(16, "0")}`)}
];

/// The primary composites, as indices into DECOMPOSITIONS, sorted by (first, second): the two-code-point decompositions
/// less the composition exclusions (CompositionExclusions.txt, non-starters and decompositions that start with one).
pub static COMPOSITIONS: [u16; ${composites.length}] = [
${rows(composites, 16, ({ index }) => String(index))}
];
`;
}

async function main() {
	const check = process.argv.includes("--check");
	const [unicodeData, exclusions] = await Promise.all(FILES.map(pinned));
	const text = render(table(unicodeData, exclusions));
	if (check) {
		const current = fs.existsSync(OUT) ? fs.readFileSync(OUT, "utf8").replace(/\r\n/g, "\n") : "";
		if (current !== text) {
			console.error(`unicode-normalization: ${path.relative(ROOT, OUT)} is not what Unicode ${VERSION} makes; run node tools/unicode-normalization.mjs`);
			process.exit(1);
		}
		console.log(`unicode-normalization: ${path.relative(ROOT, OUT)} matches Unicode ${VERSION}`);
		return;
	}
	fs.writeFileSync(OUT, text);
	console.log(`unicode-normalization: wrote ${path.relative(ROOT, OUT)} (Unicode ${VERSION})`);
}

main().catch(error => {
	console.error(`unicode-normalization: ${error.message}`);
	process.exit(1);
});

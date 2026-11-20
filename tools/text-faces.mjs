#!/usr/bin/env node
// data/text-faces.json: how TextService:GetTextBoundsAsync without a text layer (src/text_measure.rs) measures a Font
// face that is not one of the Enum.Font faces ree's tables hold. Runs tools/text-faces.luau in a playing client through
// the parity capture command (tools/parity/README.md "Capture harness contract") and sorts each face of the families
// the Enum.Font items name:
//   same: the face measures exactly as an Enum.Font face (a family without that weight or style picks one it has),
//         so the tables measure it;
//   near: it measures as no Enum.Font face (a face of its own the tables do not hold), or it is an Enum.Font face whose
//         GetTextSize answer the tables hold is not its GetTextBoundsAsync answer (SourceSansItalic: GetTextSize
//         measures it upright); ree measures it as the Enum.Font face of its family nearest in these widths and
//         reports it missing.
//
//   REE_PARITY_CAPTURE="<command>" node tools/text-faces.mjs     # capture, then write data/text-faces.json
//   node tools/text-faces.mjs --from <outputs.json>              # write from a capture's output
//   ... --keep <outputs.json>                                     # also keep the capture's output there
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..");
const OUT = path.join(REPO, "data", "text-faces.json");

function capture() {
	const command = process.env.REE_PARITY_CAPTURE;
	if (!command) {
		console.error("text-faces: set REE_PARITY_CAPTURE to the parity capture command, or pass --from <outputs.json>");
		process.exit(2);
	}
	const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "ree-text-faces-"));
	const input = path.join(scratch, "bodies.json");
	const output = path.join(scratch, "outputs.json");
	fs.writeFileSync(input, JSON.stringify([{ name: "text_faces", body: fs.readFileSync(path.join(HERE, "text-faces.luau"), "utf8") }]));
	const result = spawnSync(`${command} "${input}" "${output}"`, { stdio: "inherit", shell: true, windowsHide: true, cwd: REPO });
	if (result.status !== 0 || !fs.existsSync(output)) {
		console.error(`text-faces: the capture command failed (exit ${result.status})`);
		process.exit(1);
	}
	const captured = fs.readFileSync(output, "utf8");
	fs.rmSync(scratch, { recursive: true, force: true });
	return captured;
}

function measures(text) {
	return text.split(" ").map(part => part.split("x").map(Number));
}

function build(lines) {
	const enumFaces = JSON.parse(fs.readFileSync(path.join(REPO, "data", "font-enums.json"), "utf8")).fromEnum;
	const faceOf = Object.fromEntries(Object.entries(enumFaces).map(([name, [family, weight, style]]) => [name, `${family}|${weight} ${style}`]));
	const enumOfFace = Object.fromEntries(Object.entries(faceOf).map(([name, face]) => [face, name]));
	// each Enum.Font face's bounds, and whether the tables (GetTextSize's answers) hold them
	const enums = {};
	for (const line of lines.filter(line => line.startsWith("enum "))) {
		const [head, bounds, size] = line.split(" | ");
		const name = head.slice(5);
		if (!faceOf[name]) throw new Error(`text-faces: ${name} is not in data/font-enums.json`);
		if (bounds.includes("error")) throw new Error(`text-faces: ${name} did not measure: ${line}`);
		enums[name] = { family: faceOf[name].split("|")[0], bounds, held: bounds === size };
	}
	const same = {}, near = {};
	const put = (table, family, face, name) => ((table[family] ??= {})[face] = name);
	for (const line of lines.filter(line => line.startsWith("face "))) {
		const [head, bounds] = line.split(" | ");
		const [family, weight, style] = head.slice(5).split(" ");
		const face = `${weight} ${style}`;
		const own = enumOfFace[`${family}|${face}`];
		if (own && enums[own]?.held) continue; // an Enum.Font face the tables hold
		if (bounds.includes("error")) throw new Error(`text-faces: ${head} did not measure`);
		const candidates = Object.entries(enums).filter(([, entry]) => entry.family === family && entry.held);
		if (!candidates.length) throw new Error(`text-faces: no Enum.Font face of ${family} measures as the tables do`);
		const match = candidates.find(([, entry]) => entry.bounds === bounds);
		if (match) {
			put(same, family, face, match[0]);
			continue;
		}
		// the nearest: the least summed difference over the measured widths and heights
		const mine = measures(bounds).flat();
		const distance = entry => measures(entry.bounds).flat().reduce((sum, value, index) => sum + Math.abs(value - mine[index]), 0);
		const [nearest] = candidates.sort(([a, x], [b, y]) => distance(x) - distance(y) || a.localeCompare(b))[0];
		put(near, family, face, nearest);
	}
	const sorted = table => Object.fromEntries(Object.keys(table).sort().map(family => [family, table[family]]));
	return { same: sorted(same), near: sorted(near) };
}

function main() {
	const from = process.argv.indexOf("--from");
	const text = from > 0 ? fs.readFileSync(process.argv[from + 1], "utf8") : capture();
	const keep = process.argv.indexOf("--keep");
	if (keep > 0) fs.writeFileSync(process.argv[keep + 1], text);
	const captured = JSON.parse(text);
	const entry = captured.probes?.text_faces;
	if (!entry?.ok) {
		console.error(`text-faces: not captured (${entry ? entry.error : "missing"})`);
		process.exit(1);
	}
	const table = { engine: entry.engine || captured.engine || "unknown", captured: captured.date || "unknown", ...build(entry.lines) };
	fs.writeFileSync(OUT, JSON.stringify(table, null, "\t") + "\n");
	const count = part => Object.values(table[part]).reduce((sum, faces) => sum + Object.keys(faces).length, 0);
	console.log(`text-faces: ${count("same")} faces measure as an Enum.Font face, ${count("near")} do not (${table.engine}, ${table.captured})`);
}

main();

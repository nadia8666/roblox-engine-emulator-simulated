#!/usr/bin/env node
// What a refresh of data/reflection.json changes: classes, members, defaults and enum items that came, went or
// changed between two dumps of tools/dump-reflection.luau. Run it before committing a new dump, and decide for each
// line whether the engine emulates it or reports it as missing (the notes list the outcome of each refresh).
//
//   node tools/reflection-diff.mjs <old.json> <new.json> [--json]
//   git show HEAD:data/reflection.json > old.json && node tools/reflection-diff.mjs old.json data/reflection.json

import fs from "node:fs";

const args = process.argv.slice(2);
const asJson = args.includes("--json");
const [oldFile, newFile] = args.filter(arg => !arg.startsWith("--"));
if (!oldFile || !newFile) {
	process.stderr.write("usage: node tools/reflection-diff.mjs <old.json> <new.json> [--json]\n");
	process.exit(2);
}

const read = file => JSON.parse(fs.readFileSync(file, "utf8"));
const before = read(oldFile);
const after = read(newFile);

const byName = list => new Map((list || []).map(item => [item.name, item]));
// Key order differs between dumps (Studio's JSONEncode), so values compare by a sorted form.
const canonical = value => {
	if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
	if (value && typeof value === "object") {
		return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
	}
	return JSON.stringify(value);
};
const show = value => {
	if (value === undefined) return "(none)";
	if (value && typeof value === "object" && value.$t === "EnumItem") return `Enum.${value.e}.${value.n}`;
	if (value && typeof value === "object" && value.$t) return `${value.$t}(${canonical(value.v ?? value.s ?? "")})`;
	return canonical(value);
};
const typeText = type => (type ? [type.script, type.enum].filter(Boolean).join(":") || type.engine || "?" : "?");
const signature = member => `(${(member.parameters || []).map(p => `${p.name}: ${typeText(p.type)}`).join(", ")})${member.returns ? ` -> ${typeText(member.returns)}` : ""}`;

const result = {
	versions: [before.version, after.version],
	counts: {
		classes: [before.classes.length, after.classes.length],
		enums: [before.enums.length, after.enums.length],
	},
	classesAdded: [],
	classesRemoved: [],
	classesChanged: [],
	enumsAdded: [],
	enumsRemoved: [],
	enumsChanged: [],
};

const oldClasses = byName(before.classes);
const newClasses = byName(after.classes);
for (const [name, entry] of newClasses) {
	if (!oldClasses.has(name)) {
		result.classesAdded.push({
			name,
			superclass: entry.superclass,
			creatable: entry.defaults !== undefined,
			properties: entry.properties.map(p => p.name),
			methods: entry.methods.map(m => m.name),
			events: entry.events.map(e => e.name),
		});
	}
}
for (const name of oldClasses.keys()) if (!newClasses.has(name)) result.classesRemoved.push(name);

for (const [name, entry] of newClasses) {
	const old = oldClasses.get(name);
	if (!old) continue;
	const change = { name, lines: [] };
	if (old.superclass !== entry.superclass) change.lines.push(`superclass ${old.superclass} -> ${entry.superclass}`);
	if ((old.defaults !== undefined) !== (entry.defaults !== undefined)) {
		change.lines.push(entry.defaults !== undefined ? "now creatable (Instance.new)" : "no longer creatable (Instance.new)");
	}
	for (const key of ["serialized"]) {
		if (canonical(old[key]) !== canonical(entry[key])) change.lines.push(`${key} ${canonical(old[key])} -> ${canonical(entry[key])}`);
	}
	for (const key of ["permits", "display"]) {
		if (canonical(old[key]) !== canonical(entry[key])) change.lines.push(`${key} ${canonical(old[key])} -> ${canonical(entry[key])}`);
	}
	for (const [kind, label] of [["properties", "property"], ["methods", "method"], ["events", "event"], ["callbacks", "callback"]]) {
		const olds = byName(old[kind]);
		const news = byName(entry[kind]);
		for (const [member, value] of news) {
			if (!olds.has(member)) {
				const detail = kind === "properties" ? `: ${typeText(value.type)}` : signature(value);
				const permits = value.permits ? ` ${canonical(value.permits)}` : "";
				const deprecated = value.display && value.display.DeprecationMessage !== undefined ? " [deprecated]" : "";
				change.lines.push(`+ ${label} ${member}${detail}${permits}${deprecated}`);
				continue;
			}
			const was = olds.get(member);
			if (canonical(was) === canonical(value)) continue;
			const parts = [];
			if (kind === "properties") {
				if (canonical(was.type) !== canonical(value.type)) parts.push(`type ${typeText(was.type)} -> ${typeText(value.type)}`);
				if (was.serialized !== value.serialized) parts.push(`serialized ${was.serialized} -> ${value.serialized}`);
			} else if (signature(was) !== signature(value)) {
				parts.push(`${signature(was)} -> ${signature(value)}`);
			}
			if (canonical(was.permits) !== canonical(value.permits)) parts.push(`permits ${canonical(was.permits)} -> ${canonical(value.permits)}`);
			if (canonical(was.display) !== canonical(value.display)) parts.push(`display ${canonical(was.display)} -> ${canonical(value.display)}`);
			if (parts.length === 0) parts.push("changed");
			change.lines.push(`~ ${label} ${member}: ${parts.join("; ")}`);
		}
		for (const member of olds.keys()) if (!news.has(member)) change.lines.push(`- ${label} ${member}`);
	}
	if (old.defaults && entry.defaults) {
		const names = new Set([...Object.keys(old.defaults), ...Object.keys(entry.defaults)]);
		for (const property of [...names].sort()) {
			const was = old.defaults[property];
			const now = entry.defaults[property];
			if (canonical(was) !== canonical(now)) change.lines.push(`default ${property}: ${show(was)} -> ${show(now)}`);
		}
	}
	if (change.lines.length) result.classesChanged.push(change);
}

const oldEnums = byName(before.enums);
const newEnums = byName(after.enums);
for (const [name, entry] of newEnums) {
	const old = oldEnums.get(name);
	if (!old) {
		result.enumsAdded.push({ name, items: entry.items.map(item => `${item.name}=${item.value}`) });
		continue;
	}
	const lines = [];
	const olds = byName(old.items);
	const news = byName(entry.items);
	for (const [item, value] of news) {
		if (!olds.has(item)) lines.push(`+ ${item}=${value.value}`);
		else if (olds.get(item).value !== value.value) lines.push(`~ ${item}: ${olds.get(item).value} -> ${value.value}`);
	}
	for (const item of olds.keys()) if (!news.has(item)) lines.push(`- ${item}=${olds.get(item).value}`);
	const kept = list => list.filter(item => olds.has(item.name) && news.has(item.name)).map(item => item.name).join(",");
	if (kept(old.items) !== kept(entry.items)) lines.push("order of GetEnumItems changed");
	if (lines.length) result.enumsChanged.push({ name, lines });
}
for (const name of oldEnums.keys()) if (!newEnums.has(name)) result.enumsRemoved.push(name);

if (asJson) {
	process.stdout.write(JSON.stringify(result, null, "\t") + "\n");
} else {
	const out = [];
	out.push(`${before.version} -> ${after.version}`);
	out.push(`classes ${before.classes.length} -> ${after.classes.length}, enums ${before.enums.length} -> ${after.enums.length}`);
	out.push("", `## Classes added (${result.classesAdded.length})`);
	for (const entry of result.classesAdded) {
		out.push(`${entry.name} : ${entry.superclass}${entry.creatable ? " (creatable)" : ""}`);
		if (entry.properties.length) out.push(`  properties: ${entry.properties.join(", ")}`);
		if (entry.methods.length) out.push(`  methods: ${entry.methods.join(", ")}`);
		if (entry.events.length) out.push(`  events: ${entry.events.join(", ")}`);
	}
	out.push("", `## Classes removed (${result.classesRemoved.length})`, ...result.classesRemoved);
	out.push("", `## Classes changed (${result.classesChanged.length})`);
	for (const change of result.classesChanged) out.push(change.name, ...change.lines.map(line => `  ${line}`));
	out.push("", `## Enums added (${result.enumsAdded.length})`);
	for (const entry of result.enumsAdded) out.push(`${entry.name}: ${entry.items.join(", ")}`);
	out.push("", `## Enums removed (${result.enumsRemoved.length})`, ...result.enumsRemoved);
	out.push("", `## Enums changed (${result.enumsChanged.length})`);
	for (const change of result.enumsChanged) out.push(change.name, ...change.lines.map(line => `  ${line}`));
	process.stdout.write(out.join("\n") + "\n");
}

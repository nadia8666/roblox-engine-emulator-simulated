// The release archive: SOURCE.md and ree-<version>-<os>-<arch>.zip.
//
//   node tools/release/package.mjs --binary <path> --version <version> --os <os> --arch <arch> \
//       --licenses <file> --out <dir> [--commit <SHA>]
//
// The zip is flat and holds the binary first, with Unix mode 0755, then the documents with mode 0644. Toolchain
// managers pick the executable by its position, suffix and mode (docs/building.md), so nothing else may look
// like one. Node's zlib deflates, and the entries have a fixed order and date, so the same inputs give the same zip.
import fs from "node:fs";
import path from "node:path";
import { deflateRawSync, inflateRawSync } from "node:zlib";
import { BINARY, ROOT, PLATFORMS, args, command, isMain, main, sourceText, version } from "./common.mjs";

// The documents after the binary, in archive order
export const DOCUMENTS = ["LICENSE", "LICENSE-GPL-3.0.txt", "NOTICE", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.txt", "SOURCE.md"];

// Unix file type and permission bits, kept in the upper half of a zip entry's external attributes
export const EXECUTABLE_MODE = 0o100755;
export const DOCUMENT_MODE = 0o100644;

const LOCAL_HEADER = 0x04034b50;
const CENTRAL_HEADER = 0x02014b50;
const END_OF_DIRECTORY = 0x06054b50;
const MADE_BY_UNIX = 0x314; // "made by" Unix (3), zip version 2.0 (20)
const NEEDS_VERSION = 20;
const UTF8_NAMES = 0x800;
const DEFLATED = 8;
const DOS_DATE = 0x21; // 1980-01-01, so the archive does not depend on the time it was made
const ZIP32_LIMIT = 0xffffffff;

export function crc32(bytes) {
	let crc = 0xffffffff;
	for (const byte of bytes) {
		crc ^= byte;
		for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0);
	}
	return (crc ^ 0xffffffff) >>> 0;
}

function localHeader(filename, crc, compressedSize, size) {
	const header = Buffer.alloc(30);
	header.writeUInt32LE(LOCAL_HEADER, 0);
	header.writeUInt16LE(NEEDS_VERSION, 4);
	header.writeUInt16LE(UTF8_NAMES, 6);
	header.writeUInt16LE(DEFLATED, 8);
	header.writeUInt16LE(DOS_DATE, 12); // the time at 10 stays 0
	header.writeUInt32LE(crc, 14);
	header.writeUInt32LE(compressedSize, 18);
	header.writeUInt32LE(size, 22);
	header.writeUInt16LE(filename.length, 26);
	return header;
}

function centralRecord(filename, crc, compressedSize, size, mode, offset) {
	const record = Buffer.alloc(46);
	record.writeUInt32LE(CENTRAL_HEADER, 0);
	record.writeUInt16LE(MADE_BY_UNIX, 4);
	record.writeUInt16LE(NEEDS_VERSION, 6);
	record.writeUInt16LE(UTF8_NAMES, 8);
	record.writeUInt16LE(DEFLATED, 10);
	record.writeUInt16LE(DOS_DATE, 14); // the time at 12 stays 0
	record.writeUInt32LE(crc, 16);
	record.writeUInt32LE(compressedSize, 20);
	record.writeUInt32LE(size, 24);
	record.writeUInt16LE(filename.length, 28);
	record.writeUInt32LE((mode << 16) >>> 0, 38);
	record.writeUInt32LE(offset, 42);
	return record;
}

// A zip of `entries` ({ name, bytes, mode }) in the given order: ZIP32, UTF-8 names, Unix modes
export function zip(entries) {
	const local = [];
	const central = [];
	let offset = 0;
	for (const { name, bytes, mode } of entries) {
		if (!name || /[\\/]/.test(name) || name === "." || name === "..") throw new Error(`zip entry must be flat: ${name}`);
		const filename = Buffer.from(name);
		const compressed = deflateRawSync(bytes, { level: 9 });
		const crc = crc32(bytes);
		if ([bytes.length, compressed.length, offset].some((number) => number >= ZIP32_LIMIT)) throw new Error("ZIP64 is not supported");
		const header = localHeader(filename, crc, compressed.length, bytes.length);
		local.push(header, filename, compressed);
		central.push(centralRecord(filename, crc, compressed.length, bytes.length, mode, offset), filename);
		offset += header.length + filename.length + compressed.length;
	}
	const directory = Buffer.concat(central);
	const end = Buffer.alloc(22);
	end.writeUInt32LE(END_OF_DIRECTORY, 0);
	end.writeUInt16LE(entries.length, 8);
	end.writeUInt16LE(entries.length, 10);
	end.writeUInt32LE(directory.length, 12);
	end.writeUInt32LE(offset, 16);
	return Buffer.concat([...local, directory, end]);
}

// The entries of a zip made by `zip` (no zip comment, no extra fields), each checked against its CRC. `offset` is
// where the entry's local header starts, so the binary can be checked to come first.
export function readZip(bytes) {
	const endAt = bytes.length - 22;
	if (endAt < 0 || bytes.readUInt32LE(endAt) !== END_OF_DIRECTORY) throw new Error("invalid zip end");
	const count = bytes.readUInt16LE(endAt + 10);
	let at = bytes.readUInt32LE(endAt + 16);
	const entries = [];
	for (let i = 0; i < count; i++) {
		if (bytes.readUInt32LE(at) !== CENTRAL_HEADER) throw new Error("invalid zip directory");
		const crc = bytes.readUInt32LE(at + 16);
		const compressedSize = bytes.readUInt32LE(at + 20);
		const size = bytes.readUInt32LE(at + 24);
		const nameLength = bytes.readUInt16LE(at + 28);
		const extraLength = bytes.readUInt16LE(at + 30);
		const commentLength = bytes.readUInt16LE(at + 32);
		const mode = bytes.readUInt32LE(at + 38) >>> 16;
		const offset = bytes.readUInt32LE(at + 42);
		const name = bytes.subarray(at + 46, at + 46 + nameLength).toString("utf8");

		if (bytes.readUInt32LE(offset) !== LOCAL_HEADER || bytes.readUInt16LE(offset + 8) !== DEFLATED) throw new Error("invalid zip local header");
		const localNameLength = bytes.readUInt16LE(offset + 26);
		const localExtraLength = bytes.readUInt16LE(offset + 28);
		if (bytes.subarray(offset + 30, offset + 30 + localNameLength).toString("utf8") !== name) throw new Error("zip filenames disagree");
		const dataAt = offset + 30 + localNameLength + localExtraLength;
		const content = inflateRawSync(bytes.subarray(dataAt, dataAt + compressedSize));
		if (content.length !== size || crc32(content) !== crc) throw new Error(`zip checksum: ${name}`);

		entries.push({ name, bytes: content, mode, offset });
		at += 46 + nameLength + extraLength + commentLength;
	}
	if (at !== endAt) throw new Error("zip directory size disagrees");
	return entries;
}

// Reads the zip and checks the layout every installer relies on: the files in order with the binary first, at the
// start of the archive and the only executable. Returns the entries.
export function checkArchive(bytes, executable) {
	const entries = readZip(bytes);
	const names = entries.map((entry) => entry.name);
	if (names.join("\n") !== [executable, ...DOCUMENTS].join("\n")) throw new Error("unexpected archive layout or binary order");
	const [binary, ...documents] = entries;
	const documentsAreInert = documents.every((entry) => entry.mode === DOCUMENT_MODE && !entry.name.endsWith(".exe"));
	if (binary.offset !== 0 || binary.mode !== EXECUTABLE_MODE || !documentsAreInert) throw new Error("unexpected executable modes");
	return entries;
}

if (isMain(import.meta.url)) main(() => {
	const options = args(process.argv.slice(2), ["binary", "version", "os", "arch", "licenses", "out", "commit"]);
	const platform = PLATFORMS[options.os];
	const complete = platform && platform.arch === options.arch && options.binary && options.licenses && options.out;
	if (!complete) {
		throw new Error("usage: package.mjs --binary <path> --version <version> --os <os> --arch <arch> --licenses <file> --out <dir> [--commit <SHA>]");
	}
	const releaseVersion = version();
	if (options.version !== releaseVersion) throw new Error("package version disagrees with manifests");

	// the commit that was built: the one the workflow checked out, else the current one
	const commit = options.commit || process.env.GITHUB_SHA || command("git", ["rev-parse", "HEAD"]).trim();
	const source = Buffer.from(sourceText(releaseVersion, commit));
	const contents = {
		"LICENSE": fs.readFileSync(path.join(ROOT, "LICENSE")),
		"LICENSE-GPL-3.0.txt": fs.readFileSync(path.join(ROOT, "licenses/GPL-3.0.txt")),
		"NOTICE": fs.readFileSync(path.join(ROOT, "NOTICE")),
		"THIRD_PARTY_NOTICES.md": fs.readFileSync(path.join(ROOT, "THIRD_PARTY_NOTICES.md")),
		"THIRD_PARTY_LICENSES.txt": fs.readFileSync(options.licenses),
		"SOURCE.md": source,
	};
	const entries = [
		{ name: platform.exe, bytes: fs.readFileSync(options.binary), mode: EXECUTABLE_MODE },
		...DOCUMENTS.map((name) => ({ name, bytes: contents[name], mode: DOCUMENT_MODE })),
	];
	const archive = zip(entries);
	checkArchive(archive, platform.exe);

	// SOURCE.md is written beside the zip as well as inside it
	fs.mkdirSync(options.out, { recursive: true });
	fs.writeFileSync(path.join(options.out, "SOURCE.md"), source);
	const output = path.join(options.out, `${BINARY}-${releaseVersion}-${options.os}-${options.arch}.zip`);
	fs.writeFileSync(output, archive);
	console.log(output);
});

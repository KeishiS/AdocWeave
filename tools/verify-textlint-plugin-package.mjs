import { createRequire } from "node:module";
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import process from "node:process";
import { gunzipSync } from "node:zlib";
import { pathToFileURL } from "node:url";

import { loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";
import { verifyMemoryMaximum } from "./verify-textlint-wasm-memory.mjs";

function fail(message) { throw new Error(`textlint plugin archive: ${message}`); }
function octal(bytes) {
  const text = bytes.toString("ascii").replace(/\0.*$/, "").trim();
  if (!/^[0-7]*$/.test(text)) fail("tar header has an invalid octal field");
  return text === "" ? 0 : Number.parseInt(text, 8);
}
function canonicalArchivePath(name) {
  if (name.includes("\\") || name.startsWith("/") || /^[A-Za-z]:/.test(name)) fail(`unsafe archive path: ${name}`);
  const parts = name.split("/");
  if (parts.some((part) => part === "" || part === "." || part === "..")) fail(`non-canonical archive path: ${name}`);
  if (name.normalize("NFC") !== name) fail(`non-canonical Unicode archive path: ${name}`);
  return name;
}

export function readTarMembers(tgzBytes, { maximumTarBytes = 64 * 1024 * 1024 } = {}) {
  let tar;
  try {
    tar = gunzipSync(tgzBytes, { maxOutputLength: maximumTarBytes });
  } catch (error) {
    fail(`cannot decompress archive within ${maximumTarBytes} bytes: ${error.message}`);
  }
  if (tar.length % 512 !== 0) fail("tar length is not block-aligned");
  const members = [];
  const collisionKeys = new Set();
  let ended = false;
  for (let offset = 0; offset + 512 <= tar.length;) {
    const header = tar.subarray(offset, offset + 512); offset += 512;
    if (header.every((byte) => byte === 0)) {
      if (!tar.subarray(offset).every((byte) => byte === 0)) fail("tar contains data after its end marker");
      ended = true;
      break;
    }
    const recordedChecksum = octal(header.subarray(148, 156));
    const checksumHeader = Buffer.from(header); checksumHeader.fill(0x20, 148, 156);
    if (recordedChecksum !== checksumHeader.reduce((sum, byte) => sum + byte, 0)) fail("tar header checksum does not match");
    const namePart = header.subarray(0, 100).toString("utf8").replace(/\0.*$/, "");
    const prefix = header.subarray(345, 500).toString("utf8").replace(/\0.*$/, "");
    const name = canonicalArchivePath(prefix ? `${prefix}/${namePart}` : namePart);
    const size = octal(header.subarray(124, 136));
    const type = header[156] === 0 ? "0" : String.fromCharCode(header[156]);
    if (type !== "0") fail(`unsupported tar member type ${JSON.stringify(type)}: ${name}`);
    if (!Number.isSafeInteger(size) || offset + size > tar.length) fail(`truncated tar member: ${name}`);
    const key = name.normalize("NFC").toLocaleLowerCase("en-US");
    if (collisionKeys.has(key)) fail(`duplicate or portable-name collision: ${name}`);
    collisionKeys.add(key);
    members.push({ data: Buffer.from(tar.subarray(offset, offset + size)), name, size, type });
    offset += Math.ceil(size / 512) * 512;
  }
  if (!ended) fail("tar has no end marker");
  return members;
}

function assertNoMachinePath(bytes) {
  const text = bytes.toString("latin1");
  if (/(?:^|[^A-Za-z0-9_])\/(?:workspace|home|Users|tmp|private\/tmp|builds?|runner|__w)\//.test(text) || /[A-Za-z]:\\/.test(text)) fail("WebAssembly contains a machine-specific path");
}

export async function verifyTextlintPluginPackage(archivePath, { maximumPackedBytes, maximumUnpackedBytes } = {}) {
  const contract = loadTextlintPluginPackageContract();
  const archive = resolve(archivePath);
  const packed = await readFile(archive);
  const packedLimit = maximumPackedBytes ?? contract.archive.maximumPackedBytes;
  if (packed.length > packedLimit) fail(`packed size exceeds ${packedLimit} bytes`);
  const maximumTarBytes = maximumUnpackedBytes ?? contract.archive.maximumUnpackedBytes;
  const tarOverhead = contract.archive.fileCount * (512 + 511) + 1024;
  const members = readTarMembers(packed, { maximumTarBytes: maximumTarBytes + tarOverhead });
  const expected = contract.files.map(({ path }) => `package/${path}`).sort();
  const actual = members.map(({ name }) => name).sort();
  if (members.length !== contract.archive.fileCount || JSON.stringify(actual) !== JSON.stringify(expected)) fail("file set does not match the contract");
  const unpacked = members.reduce((sum, member) => sum + member.size, 0);
  const unpackedLimit = maximumUnpackedBytes ?? contract.archive.maximumUnpackedBytes;
  if (unpacked > unpackedLimit) fail(`unpacked size exceeds ${unpackedLimit} bytes`);
  const byName = new Map(members.map((member) => [member.name.slice("package/".length), member.data]));
  const manifest = JSON.parse(byName.get("package.json").toString("utf8"));
  const release = JSON.parse(await readFile(new URL("../release-manifest.json", import.meta.url), "utf8"));
  const expectedFiles = contract.files.filter(({ path }) => path !== "package.json").map(({ path }) => path);
  const manifestKeys = ["bugs", "description", "engines", "exports", "files", "homepage", "keywords", "license", "main", "name", "peerDependencies", "private", "repository", "type", "types", "version"].sort();
  if (JSON.stringify(Object.keys(manifest).sort()) !== JSON.stringify(manifestKeys)) fail("package.json fields do not match the public allowlist");
  if (manifest.name !== contract.identity.packageName || manifest.private !== true || manifest.version !== release.packageVersion) fail("package identity does not match the contract and release");
  if (manifest.engines?.node !== contract.compatibility.nodeEngine || manifest.peerDependencies?.textlint !== contract.compatibility.textlintVersion || manifest.peerDependencies?.["@textlint/types"] !== contract.compatibility.textlintTypesVersion) fail("package compatibility does not match the contract");
  if (JSON.stringify(manifest.files) !== JSON.stringify(expectedFiles)) fail("package.json files do not match the contract");
  if (manifest.description !== "AsciiDoc Processor Plugin for textlint powered by AdocWeave" || manifest.type !== "module" ||
      manifest.main !== "./index.mjs" || manifest.types !== "./index.d.mts" ||
      JSON.stringify(manifest.exports) !== JSON.stringify({ ".": { types: "./index.d.mts", import: "./index.mjs", default: "./index.mjs" } }) ||
      JSON.stringify(manifest.keywords) !== JSON.stringify(["asciidoc", "textlint", "textlintplugin"]) ||
      manifest.license !== "MIT OR Apache-2.0" || manifest.homepage !== "https://github.com/KeishiS/adocweave" ||
      manifest.bugs !== "https://github.com/KeishiS/adocweave/issues" ||
      JSON.stringify(manifest.repository) !== JSON.stringify({ type: "git", url: "https://github.com/KeishiS/adocweave.git", directory: "packages/textlint-plugin-asciidoc" })) {
    fail("package.json public metadata does not match the allowlist");
  }
  for (const field of ["dependencies", "devDependencies", "optionalDependencies", "bundledDependencies"]) {
    const value = manifest[field];
    if (value && (Array.isArray(value) ? value.length : Object.keys(value).length) !== 0) fail(`package must not contain ${field}`);
  }
  for (const name of ["preinstall", "install", "postinstall", "prepare", "prepack", "postpack"]) if (manifest.scripts?.[name]) fail(`package must not define ${name}`);
  const wasm = byName.get(contract.wasm.binaryPath);
  assertNoMachinePath(wasm);
  verifyMemoryMaximum(wasm, contract.wasm.maximumMemoryBytes);
  const root = await mkdtemp(join(tmpdir(), "adocweave-textlint-archive-"));
  try {
    for (const member of members) { const relative = member.name.slice("package/".length); const target = join(root, relative); await mkdir(dirname(target), { recursive: true }); await writeFile(target, member.data, { flag: "wx" }); }
    const wrapper = createRequire(import.meta.url)(join(root, contract.wasm.wrapperPath));
    if (JSON.stringify(Object.keys(wrapper).sort()) !== JSON.stringify([...contract.wasm.exportNames].sort())) fail("WebAssembly wrapper exports do not match the contract");
    const plugin = await import(`${pathToFileURL(join(root, "index.mjs")).href}?verify=${Date.now()}`);
    const extensions = new plugin.Processor({}).availableExtensions();
    if (JSON.stringify(extensions) !== JSON.stringify(contract.extensions)) fail("Processor extensions do not match the contract");
  } finally { await rm(root, { recursive: true, force: true }); }
  return { fileCount: members.length, packedBytes: packed.length, unpackedBytes: unpacked };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [archive] = process.argv.slice(2);
  if (!archive) { process.stderr.write("usage: node tools/verify-textlint-plugin-package.mjs PACKAGE_TGZ\n"); process.exit(2); }
  try { const result = await verifyTextlintPluginPackage(archive); process.stdout.write(`textlint plugin archive verified: ${result.fileCount} files\n`); }
  catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}

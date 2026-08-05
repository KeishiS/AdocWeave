import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { gzipSync } from "node:zlib";

import { loadTextlintPluginPackageContract } from "../../tools/textlint-plugin-package-contract.mjs";
import { readTarMembers, verifyTextlintPluginPackage } from "../../tools/verify-textlint-plugin-package.mjs";

const contract = loadTextlintPluginPackageContract();

test("契約どおりのarchiveを検査する", async () => withArchive(entries(), async (archive) => {
  const result = await verifyTextlintPluginPackage(archive);
  assert.equal(result.fileCount, contract.archive.fileCount);
}));

test("欠落fileと余分なfileを拒否する", async () => {
  await withArchive(entries().slice(1), (archive) => assert.rejects(verifyTextlintPluginPackage(archive), /file set/));
  await withArchive([...entries(), regular("package/extra.txt", "extra")], (archive) => assert.rejects(verifyTextlintPluginPackage(archive), /file set/));
});

test("symlink、hardlink、deviceを拒否する", async () => {
  for (const type of ["2", "1", "3"]) {
    const mutant = entries(); mutant[0] = { ...mutant[0], type };
    await withArchive(mutant, (archive) => assert.rejects(verifyTextlintPluginPackage(archive), /unsupported tar member type/));
  }
});

test("絶対path、親参照、backslash、drive、caseおよびUnicode衝突を拒否する", () => {
  for (const names of [
    ["/package/a"], ["package/../a"], [String.raw`package\a`], [String.raw`C:\package\a`],
    ["package/a", "package/A"], ["package/café", "package/cafe\u0301"],
  ]) assert.throws(() => readTarMembers(tarGz(names.map((name) => regular(name, "x")))), /path|collision/);
});

test("圧縮size budgetは境界を含み、1 byte超過を拒否する", async () => withArchive(entries(), async (archive, bytes) => {
  const unpacked = entries().reduce((sum, entry) => sum + entry.data.length, 0);
  await verifyTextlintPluginPackage(archive, { maximumPackedBytes: bytes.length, maximumUnpackedBytes: unpacked });
  await assert.rejects(verifyTextlintPluginPackage(archive, { maximumPackedBytes: bytes.length - 1 }), /packed size/);
  await assert.rejects(verifyTextlintPluginPackage(archive, { maximumUnpackedBytes: unpacked - 1 }), /unpacked size/);
}));

test("WebAssemblyの機械固有pathを拒否する", async () => {
  const mutant = entries().map((entry) => entry.name.endsWith(".wasm")
    ? { ...entry, data: Buffer.concat([minimalWasm(), Buffer.from("/workspace/secret/source.rs")]) }
    : entry);
  await withArchive(mutant, (archive) => assert.rejects(verifyTextlintPluginPackage(archive), /machine-specific path/));
});

test("公開manifestの未知fieldを拒否する", async () => {
  const mutant = entries().map((entry) => entry.name === "package/package.json"
    ? { ...entry, data: Buffer.from(JSON.stringify({ ...JSON.parse(entry.data), sourceOnly: true })) }
    : entry);
  await withArchive(mutant, (archive) => assert.rejects(verifyTextlintPluginPackage(archive), /public allowlist/));
});

function entries() {
  const release = JSON.parse(readFileSync(new URL("../../release-manifest.json", import.meta.url), "utf8"));
  const manifest = { name: contract.identity.packageName, version: release.packageVersion,
    description: "AsciiDoc Processor Plugin for textlint powered by AdocWeave", private: true, type: "module",
    main: "./index.mjs", types: "./index.d.mts",
    exports: { ".": { types: "./index.d.mts", import: "./index.mjs", default: "./index.mjs" } },
    files: contract.files.filter(({ path }) => path !== "package.json").map(({ path }) => path),
    engines: { node: contract.compatibility.nodeEngine }, peerDependencies: { "@textlint/types": contract.compatibility.textlintTypesVersion, textlint: contract.compatibility.textlintVersion },
    keywords: ["asciidoc", "textlint", "textlintplugin"], license: "MIT OR Apache-2.0",
    homepage: "https://github.com/KeishiS/adocweave", bugs: "https://github.com/KeishiS/adocweave/issues",
    repository: { type: "git", url: "https://github.com/KeishiS/adocweave.git", directory: "packages/textlint-plugin-asciidoc" } };
  return contract.files.map(({ path }) => regular(`package/${path}`,
    path === "package.json" ? `${JSON.stringify(manifest)}\n`
      : path === contract.wasm.wrapperPath ? "module.exports = { parseText() {} };\n"
        : path === contract.wasm.binaryPath ? minimalWasm() : sourceFor(path)));
}

import { readFileSync } from "node:fs";
function sourceFor(path) { try { return readFileSync(new URL(path === "LICENSE-APACHE" || path === "LICENSE-MIT" ? `../../${path}` : `./${path}`, import.meta.url)); } catch { return "fixture\n"; } }
function minimalWasm() { return Buffer.from([0,97,115,109,1,0,0,0,5,5,1,1,1,128,32]); }
function regular(name, data, type = "0") { return { data: Buffer.from(data), name, type }; }
async function withArchive(members, callback) { const root = await mkdtemp(join(tmpdir(), "adocweave-archive-test-")); const archive = join(root, "fixture.tgz"); const bytes = tarGz(members); try { await writeFile(archive, bytes); return await callback(archive, bytes); } finally { await rm(root, { recursive: true, force: true }); } }
function tarGz(members) { return gzipSync(Buffer.concat([...members.map(tarEntry), Buffer.alloc(1024)])); }
function tarEntry({ name, data, type = "0" }) {
  const header = Buffer.alloc(512); header.write(name, 0, 100, "utf8"); writeOctal(header, 100, 8, 0o644); writeOctal(header, 108, 8, 0); writeOctal(header, 116, 8, 0); writeOctal(header, 124, 12, data.length); writeOctal(header, 136, 12, 0); header.fill(0x20, 148, 156); header.write(type, 156, 1, "ascii"); header.write("ustar\0", 257, 6, "ascii"); header.write("00", 263, 2, "ascii"); writeOctal(header, 148, 8, [...header].reduce((sum, byte) => sum + byte, 0)); return Buffer.concat([header, data, Buffer.alloc((512 - data.length % 512) % 512)]);
}
function writeOctal(buffer, offset, length, value) { buffer.write(`${value.toString(8).padStart(length - 1, "0")}\0`, offset, length, "ascii"); }

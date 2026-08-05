import assert from "node:assert/strict";
import { mkdtemp, readFile, readdir, rm, writeFile, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { stageTextlintPluginPackage } from "../../tools/stage-textlint-plugin-package.mjs";

test("契約からpackage stageを明示的に生成する", async () => {
  const root = await mkdtemp(join(tmpdir(), "adocweave-stage-test-"));
  const wasm = join(root, "wasm"); const stage = join(root, "stage"); const notice = join(root, "notice.adoc");
  try {
    await mkdir(wasm);
    await writeFile(join(wasm, "adocweave_textlint_wasm.js"), "module.exports = { parseText() {} };\n");
    await writeFile(join(wasm, "adocweave_textlint_wasm_bg.wasm"), Buffer.from([0, 97, 115, 109]));
    await writeFile(notice, "= Notice\n");
    const { contract } = await stageTextlintPluginPackage(stage, wasm, notice);
    const files = await listFiles(stage);
    assert.deepEqual(files, contract.files.map(({ path }) => path).sort());
    const manifest = JSON.parse(await readFile(join(stage, "package.json"), "utf8"));
    assert.deepEqual(Object.keys(manifest).sort(), ["bugs", "description", "engines", "exports", "files", "homepage", "keywords", "license", "main", "name", "peerDependencies", "private", "repository", "type", "types", "version"].sort());
    assert.equal(manifest.name, contract.identity.packageName);
    assert.equal(manifest.engines.node, contract.compatibility.nodeEngine);
    assert.equal(await readFile(join(stage, "THIRD_PARTY_NOTICES.adoc"), "utf8"), "= Notice\n");
  } finally { await rm(root, { recursive: true, force: true }); }
});

async function listFiles(root, prefix = "") {
  const result = [];
  for (const entry of await readdir(join(root, prefix), { withFileTypes: true })) {
    const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) result.push(...await listFiles(root, relative));
    else result.push(relative);
  }
  return result.sort();
}

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  expectedManifestFiles,
  loadTextlintPluginPackageContract,
  validateTextlintPluginPackageContract,
} from "../../tools/textlint-plugin-package-contract.mjs";

const clone = (value) => structuredClone(value);

test("公開metadataを契約だけに置き、source manifestを開発用に限定する", async () => {
  const contract = loadTextlintPluginPackageContract();
  const manifest = JSON.parse(await readFile(new URL("./package.json", import.meta.url), "utf8"));
  assert.equal(manifest.name, "adocweave-textlint-plugin-development");
  assert.equal(manifest.private, true);
  assert.equal(manifest.files, undefined);
  assert.equal(manifest.engines, undefined);
  assert.equal(manifest.peerDependencies, undefined);
  assert.equal(manifest.devDependencies, undefined);
  assert.equal(expectedManifestFiles(contract).length, contract.archive.fileCount - 1);
});

test("全階層の未知fieldを拒否する", () => {
  const base = loadTextlintPluginPackageContract();
  for (const mutate of [
    (value) => { value.unknown = true; },
    (value) => { value.identity.unknown = true; },
    (value) => { value.compatibility.unknown = true; },
    (value) => { value.files[0].unknown = true; },
    (value) => { value.wasm.unknown = true; },
    (value) => { value.archive.unknown = true; },
    (value) => { value.e2eMatrix[0].unknown = true; },
    (value) => { value.oneShot.unknown = true; },
  ]) {
    const mutant = clone(base); mutate(mutant);
    assert.throws(() => validateTextlintPluginPackageContract(mutant), /unknown or missing fields/);
  }
});

test("重複pathと不正な境界値を拒否する", () => {
  const base = loadTextlintPluginPackageContract();
  const duplicate = clone(base); duplicate.files[1].path = duplicate.files[0].path;
  assert.throws(() => validateTextlintPluginPackageContract(duplicate), /duplicate file path/);
  const count = clone(base); count.archive.fileCount += 1;
  assert.throws(() => validateTextlintPluginPackageContract(count), /files.length/);
  const memory = clone(base); memory.wasm.maximumMemoryBytes += 1;
  assert.throws(() => validateTextlintPluginPackageContract(memory), /WebAssembly pages/);
});

test("package名、対応版および単発実行設定の形式を限定する", () => {
  const base = loadTextlintPluginPackageContract();
  for (const mutate of [
    (value) => { value.identity.packageName = "file:package.tgz"; },
    (value) => { value.identity.pluginName = "--plugin"; },
    (value) => { value.compatibility.nodeEngine = ">=20"; },
    (value) => { value.compatibility.textlintVersion = "latest"; },
    (value) => { value.compatibility.textlintTypesVersion = "^15.8.0"; },
    (value) => { value.oneShot.rulePackage = "https://example.com/rule.tgz"; },
    (value) => { value.oneShot.ruleVersion = "latest"; },
    (value) => { value.oneShot.preset = "../preset"; },
  ]) {
    const mutant = clone(base);
    mutate(mutant);
    assert.throws(() => validateTextlintPluginPackageContract(mutant), /invalid format/);
  }
});

test("契約pathの非canonical表現とportable衝突を拒否する", () => {
  const base = loadTextlintPluginPackageContract();
  for (const path of ["/absolute", "../parent", "a/./b", "a//b", "a/", String.raw`C:\\build`, String.raw`a\\b`, "cafe\u0301"]) {
    const mutant = clone(base); mutant.files[0].path = path;
    assert.throws(() => validateTextlintPluginPackageContract(mutant), /canonical relative path/);
  }
  const collision = clone(base); collision.files[1].path = collision.files[0].path.toLowerCase();
  assert.throws(() => validateTextlintPluginPackageContract(collision), /collision/);
});

test("schemaは全objectで追加fieldを拒否する", async () => {
  const schema = JSON.parse(await readFile(new URL("../../release/textlint-plugin-package-contract.schema.json", import.meta.url), "utf8"));
  assert.equal(schema.additionalProperties, false);
  for (const definition of Object.values(schema.$defs)) {
    if (definition.type === "object") assert.equal(definition.additionalProperties, false);
    for (const choice of definition.oneOf ?? []) assert.equal(choice.additionalProperties, false);
  }
});

test("WebAssembly構築上限をpackage contractから読み込む", async () => {
  const script = await readFile(
    new URL("../../tools/build-textlint-wasm-node.sh", import.meta.url),
    "utf8",
  );
  assert.match(script, /loadTextlintPluginPackageContract\(\)\.wasm\.maximumMemoryBytes/);
  assert.doesNotMatch(script, /maximum_memory_bytes=268435456/);
});

test("公開利用手順のpackage名と対応版をpackage contractへ同期する", async () => {
  const contract = loadTextlintPluginPackageContract();
  const match = contract.compatibility.nodeEngine.match(/^>=(\d+\.\d+\.\d+) <(\d+)$/);
  assert.ok(match);
  const documents = await Promise.all([
    readFile(new URL("../../docs/user-guide/release-installation.adoc", import.meta.url), "utf8"),
    readFile(new URL("./README.adoc", import.meta.url), "utf8"),
  ]);
  for (const document of documents) {
    assert.match(document, new RegExp(escapeRegExp(contract.identity.packageName)));
    assert.match(document, new RegExp(`textlint@${escapeRegExp(contract.compatibility.textlintVersion)}`));
    assert.match(document, new RegExp(`Node\\.js ${escapeRegExp(match[1])}以上${match[2]}未満`));
    assert.match(
      document,
      new RegExp(`--package=${escapeRegExp(contract.oneShot.rulePackage)}@${escapeRegExp(contract.oneShot.ruleVersion)}`),
    );
  }
});

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

import { loadTextlintPluginPackageContract } from "../../tools/textlint-plugin-package-contract.mjs";

const manifest = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));
const bridge = readFileSync(new URL("./bridge.mjs", import.meta.url), "utf8");
const contract = loadTextlintPluginPackageContract();

test("registryへ公開せずruntime npm依存を持たない", () => {
  assert.equal(manifest.name, "adocweave-textlint-plugin-development");
  assert.equal(manifest.private, true);
  assert.equal(manifest.engines, undefined);
  assert.equal(manifest.files, undefined);
  assert.equal(manifest.peerDependencies, undefined);
  assert.deepEqual(manifest.dependencies, undefined);
});

test("versionとWASMをpackage相対で読み込む", () => {
  assert.match(bridge, /new URL\("\.\/package\.json", import\.meta\.url\)/);
  assert.match(bridge, /require\("\.\/wasm\/adocweave_textlint_wasm\.cjs"\)/);
  assert.doesNotMatch(bridge, /release-manifest|target\//);
});

test("package境界へプロジェクト固有設定を含めない", () => {
  const serialized = JSON.stringify(manifest);
  for (const name of ["japanese-terminology", "preset-ja", "targets.json"]) {
    assert.doesNotMatch(serialized, new RegExp(name));
  }
  for (const license of ["LICENSE-APACHE", "LICENSE-MIT"]) {
    assert.equal(existsSync(new URL(`./${license}`, import.meta.url)), true);
  }
  assert.ok(contract.files.some(({ path }) => path === "THIRD_PARTY_NOTICES.adoc"));
  assert.equal(existsSync(new URL("./THIRD_PARTY_NOTICES.adoc", import.meta.url)), false);
});

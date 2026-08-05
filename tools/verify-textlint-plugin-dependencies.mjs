import { readFileSync } from "node:fs";

import { fetchedSafely } from "./npm-lock-policy.mjs";
import { loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";

const manifest = JSON.parse(
  readFileSync("packages/textlint-plugin-asciidoc/package.json", "utf8"),
);
const consumerManifest = JSON.parse(
  readFileSync("tools/textlint-plugin-e2e/package.json", "utf8"),
);
const lock = JSON.parse(
  readFileSync("tools/textlint-plugin-e2e/package-lock.json", "utf8"),
);
const recorded = JSON.parse(
  readFileSync("security/textlint-plugin-e2e-build-licenses.json", "utf8"),
);
const contract = loadTextlintPluginPackageContract();
const fixedDependencies = {
  "@textlint/types": contract.compatibility.textlintTypesVersion,
  textlint: contract.compatibility.textlintVersion,
};

if (
  JSON.stringify(manifest) !== JSON.stringify({
    name: "adocweave-textlint-plugin-development",
    version: "0.0.0",
    private: contract.identity.private,
    type: "module",
  }) ||
  lock.lockfileVersion !== 3
) {
  throw new Error("textlint pluginのmanifestまたはconsumer lockfileを解釈できません");
}
const consumer = lock.packages?.[""];
if (consumer?.name !== "@adocweave/textlint-plugin-e2e" || consumer?.version !== "0.0.0" ||
    JSON.stringify(consumerManifest.dependencies) !== JSON.stringify(fixedDependencies) ||
    JSON.stringify(consumer?.dependencies) !== JSON.stringify(fixedDependencies)) {
  throw new Error("textlint pluginの固定consumer依存を解釈できません");
}
for (const field of ["dependencies", "optionalDependencies", "bundledDependencies"]) {
  const value = manifest[field];
  if (value && (Array.isArray(value) ? value.length : Object.keys(value).length) !== 0) {
    throw new Error(`textlint pluginに実行時依存があります: ${field}`);
  }
}
for (const name of ["preinstall", "install", "postinstall", "prepare", "prepack", "postpack"]) {
  if (manifest.scripts?.[name]) throw new Error(`textlint pluginに禁止されたscriptがあります: ${name}`);
}

let packageCount = 0;
const observed = new Set();
for (const [path, entry] of Object.entries(lock.packages)) {
  if (!path) continue;
  if (!fetchedSafely(entry)) {
    throw new Error(`textlint plugin依存の取得元またはintegrityが許可境界に適合しません: ${path}`);
  }
  const license = entry.license ?? recorded.overrides?.[path];
  if (typeof license !== "string" || license.length === 0) {
    throw new Error(`textlint plugin依存のライセンスを確認できません: ${path}`);
  }
  observed.add(license);
  packageCount += 1;
}
assertCatalog(recorded, observed);

function assertCatalog(catalog, actual) {
  if (catalog.schemaVersion !== 1 || !Array.isArray(catalog.licenses)) {
    throw new Error("textlint plugin依存のライセンス目録を解釈できません");
  }
  const expected = [...catalog.licenses].sort();
  const found = [...actual].sort();
  if (JSON.stringify(expected) !== JSON.stringify(found)) {
    throw new Error("textlint plugin依存のライセンス目録が実際と一致しません");
  }
}

process.stdout.write(`textlint plugin dependency boundaryを検証しました: ${packageCount} package。\n`);

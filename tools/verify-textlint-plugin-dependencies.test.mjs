import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { fetchedSafely } from "./npm-lock-policy.mjs";

const manifest = JSON.parse(readFileSync(
  new URL("../packages/textlint-plugin-asciidoc/package.json", import.meta.url),
  "utf8",
));
const lock = JSON.parse(readFileSync(
  new URL("textlint-plugin-e2e/package-lock.json", import.meta.url),
  "utf8",
));
const catalog = JSON.parse(readFileSync(
  new URL("../security/textlint-plugin-e2e-build-licenses.json", import.meta.url),
  "utf8",
));
const governance = readFileSync(new URL("dependency-governance.sh", import.meta.url), "utf8");

test("公開textlint pluginの実行時npm依存を0件に固定する", () => {
  assert.deepEqual(manifest.dependencies ?? {}, {});
  assert.deepEqual(manifest.optionalDependencies ?? {}, {});
  assert.deepEqual(manifest.bundledDependencies ?? [], []);
  assert.deepEqual(manifest.peerDependencies, {
    "@textlint/types": "15.8.0",
    textlint: "15.8.0"
  });
});

test("固定consumerの依存は安全な取得元とライセンス情報を持つ", () => {
  const entries = Object.entries(lock.packages).filter(([path]) => path);
  assert.notEqual(entries.length, 0);
  assert.deepEqual(
    entries.filter(([, entry]) => !fetchedSafely(entry)).map(([path]) => path),
    [],
  );
  const observed = new Set(entries.map(([path, entry]) => entry.license ?? catalog.overrides?.[path]));
  assert.deepEqual([...observed].sort(), [...catalog.licenses].sort());
});

test("公開textlint pluginの依存を監査する", () => {
  assert.match(governance, /^npm audit --include=dev --prefix tools\/textlint-plugin-e2e$/m);
  assert.match(governance, /^node tools\/verify-textlint-plugin-dependencies\.mjs$/m);
});

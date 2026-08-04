import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { fetchedSafely } from "./npm-lock-policy.mjs";

const lock = JSON.parse(
  readFileSync(new URL("../tools/textlint/package-lock.json", import.meta.url), "utf8")
);
const catalog = JSON.parse(
  readFileSync(new URL("../security/textlint-build-licenses.json", import.meta.url), "utf8")
);
const governance = readFileSync(new URL("dependency-governance.sh", import.meta.url), "utf8");

test("textlint依存の取得元とintegrityを固定する", () => {
  const violations = Object.entries(lock.packages)
    .filter(([path]) => path)
    .filter(([, entry]) => !fetchedSafely(entry))
    .map(([path]) => path);
  assert.deepEqual(violations, []);
});

test("textlint依存のライセンス目録が実際と一致する", () => {
  const observed = new Set();
  for (const [path, entry] of Object.entries(lock.packages)) {
    if (!path) continue;
    observed.add(entry.license ?? catalog.overrides[path]);
  }
  assert.deepEqual([...catalog.licenses].sort(), [...observed].sort());
});

test("textlintの開発用依存を監査する", () => {
  assert.match(governance, /^npm audit --include=dev --prefix tools\/textlint$/m);
  assert.match(governance, /^node tools\/verify-textlint-dependencies\.mjs$/m);
});

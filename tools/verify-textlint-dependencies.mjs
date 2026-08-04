import { readFileSync } from "node:fs";

import { fetchedSafely } from "./npm-lock-policy.mjs";

const manifest = JSON.parse(readFileSync("tools/textlint/package.json", "utf8"));
const lock = JSON.parse(readFileSync("tools/textlint/package-lock.json", "utf8"));
const recorded = JSON.parse(
  readFileSync("security/textlint-build-licenses.json", "utf8")
);

if (
  manifest.private !== true ||
  lock.lockfileVersion !== 3 ||
  lock.packages?.[""]?.version !== manifest.version ||
  Object.keys(manifest.dependencies ?? {}).length !== 0
) {
  throw new Error("textlint依存境界のmanifestとlockfileが一致しません");
}
if (recorded.schemaVersion !== 1) {
  throw new Error("textlint依存のライセンス目録を解釈できません");
}

const observed = new Set();
for (const [path, entry] of Object.entries(lock.packages)) {
  if (!path) continue;
  if (!fetchedSafely(entry)) {
    throw new Error(`textlint依存の取得元またはintegrityが許可境界に適合しません: ${path}`);
  }
  const license = entry.license ?? recorded.overrides?.[path];
  if (typeof license !== "string" || license.length === 0) {
    throw new Error(`textlint依存のライセンスを確認できません: ${path}`);
  }
  observed.add(license);
}

const expected = [...recorded.licenses].sort();
const actual = [...observed].sort();
if (expected.length !== actual.length || expected.some((value, index) => value !== actual[index])) {
  throw new Error(
    `textlint依存のライセンス目録が実際と一致しません: expected=${expected.join(",")} actual=${actual.join(",")}`
  );
}

process.stdout.write(
  `textlint dependency boundaryを検証しました: ${Object.keys(lock.packages).length - 1} package、${actual.length} 種のライセンス。\n`
);

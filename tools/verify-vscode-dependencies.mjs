import { readFileSync } from "node:fs";

import { fetchedSafely, validSha512Integrity } from "./npm-lock-policy.mjs";

export { validSha512Integrity } from "./npm-lock-policy.mjs";

const manifest = JSON.parse(readFileSync("editors/vscode/package.json", "utf8"));
const lock = JSON.parse(readFileSync("editors/vscode/package-lock.json", "utf8"));
const buildLicenses = JSON.parse(readFileSync("security/vscode-build-licenses.json", "utf8"));

/// Licenses a package may carry when it reaches a user's machine.
const shippedLicenses = new Set([
  "Apache-2.0",
  "BlueOak-1.0.0",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "ISC",
  "MIT",
]);

if (
  manifest.private !== true ||
  lock.lockfileVersion !== 3 ||
  lock.packages?.[""]?.version !== manifest.version
) {
  throw new Error("VS Code dependency boundaryのmanifestとlockfileが一致しません");
}
if (buildLicenses.schemaVersion !== 1) {
  throw new Error("ビルド用依存のライセンス目録のschema versionが未対応です");
}

/// Every dependency is fetched and run somewhere, so origin and integrity are
/// required of all of them. What differs is the license: a shipped package
/// carries obligations to the user, while a build tool only runs here and never
/// reaches them. Reading the two boundaries as one policy meant the build tools
/// were checked as neither, since `--omit=dev` and `entry.dev === true` skipped
/// them: Biome, TypeScript, esbuild and vsce all run in CI and produce the VSIX.
const observedBuildLicenses = new Set();
for (const [path, entry] of Object.entries(lock.packages)) {
  if (!path) continue;
  if (!fetchedSafely(entry)) {
    throw new Error(`VS Code dependencyの取得元またはintegrityが許可境界に適合しません：${path}`);
  }
  if (entry.dev === true) {
    observedBuildLicenses.add(entry.license);
    continue;
  }
  if (!shippedLicenses.has(entry.license)) {
    throw new Error(`VS Code runtime dependencyのライセンスが許可境界に適合しません：${path}`);
  }
}

// The recorded set is compared exactly rather than as an allowlist. A build tool
// arriving with a license nobody has read should be seen in review, and a
// license that no dependency carries any more should leave the record.
const recorded = [...buildLicenses.licenses].sort();
const observed = [...observedBuildLicenses].sort();
if (recorded.length !== observed.length || recorded.some((value, index) => value !== observed[index])) {
  const added = observed.filter((value) => !recorded.includes(value));
  const removed = recorded.filter((value) => !observed.includes(value));
  throw new Error(
    "ビルド用依存のライセンス目録が実際と一致しません。" +
      `security/vscode-build-licenses.jsonを更新してください：追加=${added.join("、") || "なし"}` +
      ` 削除=${removed.join("、") || "なし"}`,
  );
}

process.stdout.write(
  `VS Code dependency boundaryを検証しました：配布時 ${
    Object.values(lock.packages).filter((entry) => entry.dev !== true).length - 1
  } package、ビルド用 ${observedBuildLicenses.size} 種のライセンス。\n`,
);

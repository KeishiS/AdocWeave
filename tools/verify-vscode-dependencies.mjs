import { readFileSync } from "node:fs";

const manifest = JSON.parse(readFileSync("editors/vscode/package.json", "utf8"));
const lock = JSON.parse(readFileSync("editors/vscode/package-lock.json", "utf8"));
const allowedLicenses = new Set(["Apache-2.0", "BlueOak-1.0.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "MIT"]);

if (manifest.private !== true || lock.lockfileVersion !== 3 || lock.packages?.[""]?.version !== manifest.version) {
  throw new Error("VS Code dependency boundaryのmanifestとlockfileが一致しません");
}

for (const [path, entry] of Object.entries(lock.packages)) {
  if (!path || entry.dev === true) continue;
  if (
    typeof entry.version !== "string" ||
    typeof entry.integrity !== "string" ||
    !entry.integrity.startsWith("sha512-") ||
    typeof entry.resolved !== "string" ||
    !entry.resolved.startsWith("https://registry.npmjs.org/") ||
    !allowedLicenses.has(entry.license)
  ) {
    throw new Error(`VS Code runtime dependencyが許可境界に適合しません：${path}`);
  }
}

process.stdout.write("VS Code runtime dependency boundaryを検証しました。\n");

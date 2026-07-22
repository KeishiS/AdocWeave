import { readFileSync } from "node:fs";

const ROOT = new URL("../", import.meta.url);
const baseline = JSON.parse(readFileSync(new URL("security/duplicate-dependencies.json", ROOT), "utf8"));
const inputs = process.argv.slice(2);

function fail(message) {
  throw new Error(message);
}

if (baseline.version !== 1 || inputs.length !== 2) {
  fail("usage: node tools/verify-duplicate-dependencies.mjs ROOT_METADATA ZED_METADATA");
}

function duplicates(path) {
  const metadata = JSON.parse(readFileSync(path, "utf8"));
  const workspace = new Set(metadata.workspace_members);
  const versions = new Map();
  for (const pkg of metadata.packages.filter((entry) => !workspace.has(entry.id))) {
    if (!versions.has(pkg.name)) versions.set(pkg.name, new Set());
    versions.get(pkg.name).add(pkg.version);
  }
  return [...versions]
    .filter(([, found]) => found.size > 1)
    .map(([crate, found]) => ({ crate, versions: [...found].sort() }))
    .sort((left, right) => left.crate.localeCompare(right.crate));
}

for (const [boundary, path] of [["native-and-wasm", inputs[0]], ["zed-extension", inputs[1]]]) {
  const expected = baseline.boundaries[boundary];
  if (!Array.isArray(expected) || expected.some((entry) => !entry.reason || !Array.isArray(entry.versions))) {
    fail(`invalid duplicate dependency baseline: ${boundary}`);
  }
  const normalized = expected
    .map(({ crate, versions }) => ({ crate, versions: [...versions].sort() }))
    .sort((left, right) => left.crate.localeCompare(right.crate));
  const actual = duplicates(path);
  if (JSON.stringify(actual) !== JSON.stringify(normalized)) {
    fail(`${boundary} duplicate dependencies changed: expected ${JSON.stringify(normalized)}, got ${JSON.stringify(actual)}`);
  }
}

process.stdout.write("duplicate dependency baseline verified\n");

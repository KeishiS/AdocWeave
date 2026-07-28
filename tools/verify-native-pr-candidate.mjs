import { readdirSync, readFileSync } from "node:fs";
import { basename, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

export function expectedNativePullRequestAssets(plan) {
  const nativeTargets = new Set(
    plan.targets.filter(({ os }) => os === "darwin" || os === "win32").map(({ triple }) => triple),
  );
  return plan.assets
    .filter(({ kind, target }) =>
      ["browser", "vscode", "zed"].includes(kind) || (target && nativeTargets.has(target)))
    .map(({ name }) => name)
    .sort();
}

export function verifyNativePullRequestAssets(actual, plan) {
  const expected = expectedNativePullRequestAssets(plan);
  const sortedActual = [...actual].sort();
  if (JSON.stringify(sortedActual) !== JSON.stringify(expected)) {
    throw new Error(
      `native pull request candidate mismatch:\nexpected: ${expected.join(", ")}\nactual: ${sortedActual.join(", ")}`,
    );
  }
}

function main() {
  const [candidateArgument] = process.argv.slice(2);
  if (!candidateArgument) {
    process.stderr.write("usage: node tools/verify-native-pr-candidate.mjs CANDIDATE_DIRECTORY\n");
    process.exit(2);
  }
  const candidate = resolve(candidateArgument);
  const plan = JSON.parse(
    readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"),
  );
  const entries = readdirSync(candidate, { withFileTypes: true });
  if (entries.some((entry) => !entry.isFile())) {
    throw new Error("native pull request candidate must contain files only");
  }
  verifyNativePullRequestAssets(entries.map(({ name }) => basename(name)), plan);
  process.stdout.write("native pull request candidate verified\n");
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();

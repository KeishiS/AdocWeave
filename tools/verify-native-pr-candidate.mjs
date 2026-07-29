import { readdirSync, readFileSync } from "node:fs";
import { basename, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

export function expectedPullRequestAssets(plan, { global = true, native = true } = {}) {
  const nativeTargets = new Set(
    plan.targets.filter(({ os }) => os === "darwin" || os === "win32").map(({ triple }) => triple),
  );
  return plan.assets
    .filter(({ kind, target }) =>
      (global && ["browser", "vscode", "zed"].includes(kind)) ||
      (native && target && nativeTargets.has(target)))
    .map(({ name }) => name)
    .sort();
}

export function verifyPullRequestAssets(actual, plan, selection) {
  const expected = expectedPullRequestAssets(plan, selection);
  const sortedActual = [...actual].sort();
  if (JSON.stringify(sortedActual) !== JSON.stringify(expected)) {
    throw new Error(
      `pull request candidate mismatch:\nexpected: ${expected.join(", ")}\nactual: ${sortedActual.join(", ")}`,
    );
  }
}

function main() {
  const [candidateArgument, nativeArgument = "true", globalArgument = "true"] = process.argv.slice(2);
  if (!candidateArgument) {
    process.stderr.write(
      "usage: node tools/verify-native-pr-candidate.mjs CANDIDATE_DIRECTORY NATIVE_REQUIRED GLOBAL_REQUIRED\n",
    );
    process.exit(2);
  }
  if (!["true", "false"].includes(nativeArgument) || !["true", "false"].includes(globalArgument)) {
    process.stderr.write("candidate selection arguments must be true or false\n");
    process.exit(2);
  }
  const candidate = resolve(candidateArgument);
  const plan = JSON.parse(
    readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"),
  );
  const entries = readdirSync(candidate, { withFileTypes: true });
  if (entries.some((entry) => !entry.isFile())) {
    throw new Error("pull request candidate must contain files only");
  }
  verifyPullRequestAssets(entries.map(({ name }) => basename(name)), plan, {
    global: globalArgument === "true",
    native: nativeArgument === "true",
  });
  process.stdout.write("pull request candidate verified\n");
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();

import { readFileSync, writeFileSync } from "node:fs";
import process from "node:process";
import { fileURLToPath } from "node:url";

const RELEASE_AFFECTING_ROOTS = [
  ".cargo/",
  ".github/workflows/",
  "crates/",
  "editors/",
  "release/",
  "tools/",
  "web-worker/",
];
const RELEASE_AFFECTING_FILES = new Set([
  "Cargo.lock",
  "Cargo.toml",
  "Makefile.toml",
  "README.adoc",
  "THIRD_PARTY_NOTICES.adoc",
  "flake.lock",
  "flake.nix",
  "release-manifest.json",
  "rust-toolchain.toml",
]);

function matrixEntry(target) {
  const entry = {
    target: target.triple,
    runner: target.runner,
    build: target.os === "win32" ? "windows" : "nix",
    nix: target.os === "linux",
  };
  if (target.os === "linux") {
    entry.nixSystem = target.architecture === "arm64" ? "aarch64-linux" : "x86_64-linux";
  }
  return entry;
}

export function affectsNativeCandidate(pathname) {
  return RELEASE_AFFECTING_FILES.has(pathname) ||
    RELEASE_AFFECTING_ROOTS.some((root) => pathname.startsWith(root));
}

export function nativeChangePlan(eventName, paths, distributionPlan, ref = "refs/heads/main") {
  const required = (eventName === "push" && ref === "refs/heads/main") ||
    (eventName === "pull_request" && paths.some(affectsNativeCandidate));
  const targets = distributionPlan.targets
    .filter((target) => eventName === "push" || target.os === "darwin" || target.os === "win32")
    .map(matrixEntry);
  return { required, matrix: { include: targets } };
}

function main() {
  const [eventName, ref, outputPath] = process.argv.slice(2);
  if (!eventName || !ref || !outputPath) {
    process.stderr.write("usage: node tools/native-change-plan.mjs EVENT_NAME REF GITHUB_OUTPUT\n");
    process.exit(2);
  }
  const distributionPlan = JSON.parse(
    readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"),
  );
  const paths = readFileSync(0, "utf8").replaceAll("\r\n", "\n").split("\n").filter(Boolean);
  const plan = nativeChangePlan(eventName, paths, distributionPlan, ref);
  writeFileSync(
    outputPath,
    `native_required=${plan.required}\nnative_matrix=${JSON.stringify(plan.matrix)}\n`,
    { flag: "a" },
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();

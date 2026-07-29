import { readFileSync, writeFileSync } from "node:fs";
import process from "node:process";
import { fileURLToPath } from "node:url";

const COMMON_RELEASE_ROOTS = [
  ".cargo/",
  ".github/workflows/",
  "config/",
  "release/",
];
const COMMON_RELEASE_FILES = new Set([
  "Cargo.lock",
  "Cargo.toml",
  "LICENSE-APACHE",
  "LICENSE-MIT",
  "Makefile.toml",
  "README.adoc",
  "THIRD_PARTY_NOTICES.adoc",
  "dist-workspace.toml",
  "flake.lock",
  "flake.nix",
  "release-manifest.json",
  "rust-toolchain.toml",
]);
const NATIVE_ROOTS = [
  "crates/adocweave-cli/",
  "crates/adocweave-config/",
  "crates/adocweave-host/",
  "crates/adocweave-lsp/",
  "crates/adocweave-workspace/",
  "crates/adocweave/",
];
const GLOBAL_ROOTS = [
  "crates/adocweave-config/",
  "crates/adocweave-wasm/",
  "crates/adocweave/",
  "editors/",
  "protocol/",
  "web-worker/",
];
const NON_RELEASE_ROOTS = [
  ".codex/",
  ".github/ISSUE_TEMPLATE/",
  "docs/",
  "fixtures/",
  "fuzz/",
  "security/",
];
const NON_RELEASE_FILES = new Set([
  ".adocweave.toml",
  ".gitattributes",
  ".gitignore",
  ".github/dependabot.yml",
  ".github/pull_request_template.md",
  "AGENTS.md",
  "CONTRIBUTING.adoc",
  "deny.toml",
  "tools/adoc-check.mjs",
  "tools/adoc-check.test.mjs",
  "tools/dependency-governance.sh",
  "tools/generate-third-party-notices.test.mjs",
  "tools/html5-check.mjs",
  "tools/native-change-plan.test.mjs",
  "tools/platform-contract.mjs",
  "tools/platform-contract.test.mjs",
  "tools/release-contract.test.mjs",
  "tools/release-notes.mjs",
  "tools/release-notes.test.mjs",
  "tools/release-policy.mjs",
  "tools/release-workflow-policy.mjs",
  "tools/release-workflow-policy.test.mjs",
  "tools/verify-cargo-release-metadata.mjs",
  "tools/verify-dependency-boundaries.mjs",
  "tools/verify-dependency-boundaries.test.mjs",
  "tools/verify-duplicate-dependencies.mjs",
]);
const NATIVE_TOOLS = [
  "dependency-governance.sh",
  "generate-third-party-notices.mjs",
  "install-pinned-cargo-dist.ps1",
  "local-native-check.mjs",
  "native-change-plan.mjs",
  "native-change-plan.test.mjs",
  "native-release-smoke.mjs",
  "normalize-darwin-archives.sh",
  "platform-contract.mjs",
  "platform-contract.test.mjs",
  "release-contract.mjs",
  "release-installation-e2e.mjs",
  "release-metadata.mjs",
  "release-metadata.test.mjs",
  "run-pinned-dist.sh",
  "verify-dist-plan.mjs",
  "verify-native-pr-candidate.mjs",
  "verify-native-pr-candidate.test.mjs",
];
const GLOBAL_TOOLS = [
  "browser-release-budget.mjs",
  "browser-release-budget.test.mjs",
  "browser-release-smoke.mjs",
  "browser-release-smoke.test.mjs",
  "generate-protocol.mjs",
  "generate-third-party-notices.mjs",
  "host-executable.mjs",
  "host-executable.test.mjs",
  "package-browser-release.sh",
  "package-vscode-release.sh",
  "package-zed-release.sh",
  "process-lifecycle.mjs",
  "process-lifecycle.test.mjs",
  "protocol-rust-codegen.mjs",
  "protocol-rust-codegen.test.mjs",
  "release-contract.mjs",
  "release-installation-e2e.mjs",
  "release-metadata.mjs",
  "release-metadata.test.mjs",
  "verify-dist-plan.mjs",
  "verify-vscode-dependencies.mjs",
  "zed-query-contract.mjs",
  "zed-query-nodes.json",
  "zed-release-smoke.mjs",
];

function startsWithAny(pathname, roots) {
  return roots.some((root) => pathname.startsWith(root));
}

function isCommonReleaseInput(pathname) {
  return COMMON_RELEASE_FILES.has(pathname) || startsWithAny(pathname, COMMON_RELEASE_ROOTS);
}

function isNamedTool(pathname, names) {
  if (!pathname.startsWith("tools/")) return false;
  return names.includes(pathname.slice("tools/".length));
}

export function affectsNativeCandidate(pathname) {
  return candidateImpact(pathname).native;
}

export function affectsGlobalCandidate(pathname) {
  return candidateImpact(pathname).global;
}

export function candidateImpact(pathname) {
  return classifyCandidatePath(pathname).impact;
}

export function classifyCandidatePath(pathname) {
  if (isCommonReleaseInput(pathname)) {
    return { classified: true, impact: { global: true, native: true } };
  }
  if (NON_RELEASE_FILES.has(pathname) || startsWithAny(pathname, NON_RELEASE_ROOTS)) {
    return { classified: true, impact: { global: false, native: false } };
  }
  const native = startsWithAny(pathname, NATIVE_ROOTS) || isNamedTool(pathname, NATIVE_TOOLS);
  const global = startsWithAny(pathname, GLOBAL_ROOTS) || isNamedTool(pathname, GLOBAL_TOOLS);
  if (native || global) return { classified: true, impact: { global, native } };
  // New source and build paths must receive complete candidate validation until
  // their artifact ownership is explicitly classified above.
  return { classified: false, impact: { global: true, native: true } };
}

export function auditCandidatePaths(paths) {
  return paths.filter((pathname) => !classifyCandidatePath(pathname).classified);
}

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

export function nativeChangePlan(
  eventName,
  paths,
  distributionPlan,
  ref = "refs/heads/main",
  releaseTagExists = true,
) {
  const pullRequest = eventName === "pull_request";
  const releaseMain = eventName === "push" &&
    ref === "refs/heads/main" &&
    !releaseTagExists;
  const nativeRequired = releaseMain ||
    (pullRequest && paths.some(affectsNativeCandidate));
  const globalRequired = releaseMain ||
    (pullRequest && paths.some(affectsGlobalCandidate));
  const targets = distributionPlan.targets
    .filter((target) => releaseMain || target.os === "darwin" || target.os === "win32")
    .map(matrixEntry);
  return {
    candidateRequired: nativeRequired || globalRequired,
    globalRequired,
    nativeRequired,
    preflightRequired: nativeRequired || globalRequired || ref.startsWith("refs/tags/"),
    releaseMain,
    matrix: { include: targets },
  };
}

function main() {
  if (process.argv[2] === "--audit") {
    const paths = readFileSync(0, "utf8").replaceAll("\r\n", "\n").split("\n").filter(Boolean);
    const unknown = auditCandidatePaths(paths);
    if (unknown.length > 0) {
      process.stderr.write(
        `candidate impact is not classified for tracked paths:\n${unknown.map((path) => `- ${path}`).join("\n")}\n`,
      );
      process.exit(1);
    }
    return;
  }
  const [eventName, ref, outputPath, releaseTagExistsArgument] = process.argv.slice(2);
  if (!eventName || !ref || !outputPath ||
      !["true", "false"].includes(releaseTagExistsArgument)) {
    process.stderr.write(
      "usage: node tools/native-change-plan.mjs EVENT_NAME REF GITHUB_OUTPUT RELEASE_TAG_EXISTS\n",
    );
    process.exit(2);
  }
  const distributionPlan = JSON.parse(
    readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"),
  );
  const paths = readFileSync(0, "utf8").replaceAll("\r\n", "\n").split("\n").filter(Boolean);
  const plan = nativeChangePlan(
    eventName,
    paths,
    distributionPlan,
    ref,
    releaseTagExistsArgument === "true",
  );
  writeFileSync(
    outputPath,
    [
      `candidate_required=${plan.candidateRequired}`,
      `global_required=${plan.globalRequired}`,
      `native_required=${plan.nativeRequired}`,
      `preflight_required=${plan.preflightRequired}`,
      `release_main=${plan.releaseMain}`,
      `native_matrix=${JSON.stringify(plan.matrix)}`,
      "",
    ].join("\n"),
    { flag: "a" },
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();

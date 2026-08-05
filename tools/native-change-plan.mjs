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
  "crates/adocweave-textlint/",
  "crates/adocweave-textlint-wasm/",
  "crates/adocweave-wasm/",
  "crates/adocweave/",
  "editors/",
  "packages/textlint-plugin-asciidoc/",
  "protocol/",
  "tools/textlint-plugin-e2e/",
  "web-worker/",
];
const NON_RELEASE_ROOTS = [
  ".codex/",
  ".github/ISSUE_TEMPLATE/",
  // Repository conventions checked as tests. The crate ships no library or
  // binary, so nothing here reaches a release artifact.
  "crates/adocweave-governance/",
  "docs/",
  "fixtures/",
  "fuzz/",
  "security/",
  "tools/textlint/",
];
const NON_RELEASE_FILES = new Set([
  ".adocweave.toml",
  ".gitattributes",
  ".gitignore",
  ".github/dependabot-auto-merge-policy.json",
  ".github/SECURITY.md",
  ".github/dependabot.yml",
  ".github/pull_request_template.md",
  "AGENTS.md",
  "CONTRIBUTING.adoc",
  "deny.toml",
  "tools/adoc-check.mjs",
  "tools/adoc-check.test.mjs",
  "tools/config-schema.test.mjs",
  "tools/dependency-governance.sh",
  "tools/dependabot-alert-inventory.sh",
  "tools/dependabot-alert-inventory.test.mjs",
  "tools/dependabot-alert-snapshot.sh",
  "tools/dependabot-alert-snapshot.test.mjs",
  "tools/dependabot-auto-merge-disable-result.jq",
  "tools/dependabot-auto-merge-pr-detail.jq",
  "tools/dependabot-auto-merge-pr-numbers.jq",
  "tools/dependabot-auto-merge-policy.mjs",
  "tools/dependabot-auto-merge-policy.test.mjs",
  "tools/dependabot-auto-merge-workflow.test.mjs",
  "tools/generate-third-party-notices.test.mjs",
  "tools/html5-check.mjs",
  "tools/native-change-plan.test.mjs",
  "tools/platform-contract.mjs",
  "tools/platform-contract.test.mjs",
  "tools/release-claims.mjs",
  "tools/release-claims.test.mjs",
  "tools/release-contract.test.mjs",
  "tools/release-installation-e2e.test.mjs",
  "tools/release-notes.mjs",
  "tools/release-notes.test.mjs",
  "tools/release-policy.mjs",
  "tools/sync-release-version.test.mjs",
  "tools/release-workflow-policy.mjs",
  "tools/release-workflow-policy.test.mjs",
  "tools/semver-gate.mjs",
  "tools/semver-gate.test.mjs",
  "tools/verify-cargo-release-metadata.mjs",
  "tools/verify-dependency-boundaries.mjs",
  "tools/verify-dependency-boundaries.test.mjs",
  "tools/verify-duplicate-dependencies.mjs",
  "tools/verify-vscode-dependencies.test.mjs",
  "tools/npm-lock-policy.mjs",
  "tools/verify-textlint-dependencies.mjs",
  "tools/verify-textlint-dependencies.test.mjs",
  "tools/verify-textlint-plugin-dependencies.mjs",
  "tools/verify-textlint-plugin-dependencies.test.mjs",
]);
const NATIVE_TOOLS = [
  "dependency-governance.sh",
  "generate-third-party-notices.mjs",
  "install-pinned-cargo-dist.ps1",
  "local-native-check.mjs",
  "native-change-plan.mjs",
  "native-change-plan.test.mjs",
  "native-lsp-smoke.mjs",
  "native-lsp-smoke.test.mjs",
  "native-release-smoke.mjs",
  "normalize-darwin-archives.sh",
  "platform-contract.mjs",
  "platform-contract.test.mjs",
  "release-contract.mjs",
  "release-installation-e2e.mjs",
  "release-metadata.mjs",
  "release-metadata.test.mjs",
  "run-pinned-dist.sh",
  "sync-release-version.mjs",
  "verify-dist-plan.mjs",
  "verify-native-pr-candidate.mjs",
  "verify-native-pr-candidate.test.mjs",
];
const GLOBAL_TOOLS = [
  "browser-startup.mjs",
  "browser-release-budget.mjs",
  "browser-release-budget.test.mjs",
  "browser-release-smoke.mjs",
  "browser-release-smoke.test.mjs",
  "build-textlint-wasm-node.sh",
  "generate-protocol.mjs",
  "generate-third-party-notices.mjs",
  "host-executable.mjs",
  "host-executable.test.mjs",
  "package-browser-release.sh",
  "package-textlint-plugin-release.sh",
  "package-vscode-release.sh",
  "package-zed-release.sh",
  "process-lifecycle.mjs",
  "process-lifecycle.test.mjs",
  "textlint-plugin-npx-smoke.mjs",
  "textlint-plugin-npx-smoke.test.mjs",
  "textlint-plugin-post-release-smoke.mjs",
  "textlint-plugin-post-release-smoke.test.mjs",
  "textlint-plugin-consumer-e2e.mjs",
  "textlint-plugin-compatibility-probe.mjs",
  "textlint-plugin-release-smoke.mjs",
  "textlint-plugin-release-smoke.test.mjs",
  "verify-textlint-plugin-reproducibility.mjs",
  "verify-textlint-plugin-reproducibility.test.mjs",
  "verify-textlint-plugin-package.mjs",
  "stage-textlint-plugin-package.mjs",
  "verify-textlint-wasm-memory.mjs",
  "verify-textlint-wasm-memory.test.mjs",
  "protocol-rust-codegen.mjs",
  "protocol-rust-codegen.test.mjs",
  "release-contract.mjs",
  "release-installation-e2e.mjs",
  "release-metadata.mjs",
  "release-metadata.test.mjs",
  "sync-release-version.mjs",
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

/// Paths that decide whether the Rust source checks have anything to verify.
const RUST_SOURCE_ROOTS = ["crates/", "fuzz/", "security/"];
const RUST_SOURCE_FILES = new Set([
  "Cargo.lock",
  "Cargo.toml",
  "rust-toolchain.toml",
  "deny.toml",
]);
/// Every input the dependency boundary audit reads.
///
/// The audit was previously requested by the Rust source file set, which does
/// not contain the advisory revision, the boundary and exception inventories,
/// the duplicate baseline, or the audit scripts themselves. A pull request that
/// edited one of those changed what the audit accepts and still reported
/// success without running it. `tools/native-change-plan.test.mjs` reads the
/// audit script and requires every repository path it names to appear here.
export const DEPENDENCY_AUDIT_ROOTS = [
  "security/",
  "editors/",
  "packages/textlint-plugin-asciidoc/",
  "tools/textlint/",
  "tools/textlint-plugin-e2e/",
];
export const DEPENDENCY_AUDIT_FILES = new Set([
  "Cargo.lock",
  "Cargo.toml",
  "deny.toml",
  "release/textlint-plugin-package-contract.json",
  "release/textlint-plugin-package-contract.schema.json",
  "tools/dependency-governance.sh",
  "tools/generate-third-party-notices.mjs",
  "tools/verify-dependency-boundaries.mjs",
  // `dependency-governance` runs `dependency-governance-test` first, so these
  // decide what the audit accepts just as the scripts they test do.
  "tools/verify-dependency-boundaries.test.mjs",
  "tools/verify-duplicate-dependencies.mjs",
  "tools/verify-vscode-dependencies.mjs",
  "tools/verify-vscode-dependencies.test.mjs",
  "tools/npm-lock-policy.mjs",
  "tools/package-textlint-plugin-release.sh",
  "tools/stage-textlint-plugin-package.mjs",
  "tools/verify-textlint-dependencies.mjs",
  "tools/verify-textlint-dependencies.test.mjs",
  "tools/verify-textlint-plugin-dependencies.mjs",
  "tools/verify-textlint-plugin-dependencies.test.mjs",
  "tools/verify-textlint-plugin-package.mjs",
  "tools/verify-textlint-plugin-reproducibility.mjs",
]);
/// Paths that decide whether the adapter contracts have anything to verify.
const ADAPTER_ROOTS = [
  "crates/",
  "editors/",
  "protocol/",
  "web-worker/",
  "fixtures/",
  "packages/textlint-plugin-asciidoc/",
  "tools/textlint/",
];
/// Paths whose authored AsciiDoc or generated HTML the document checks read.
const DOCUMENT_ROOTS = ["docs/", "fixtures/", "packages/textlint-plugin-asciidoc/", "tools/textlint/"];
const DOCUMENT_FILES = new Set([
  "README.adoc",
  "CONTRIBUTING.adoc",
  "THIRD_PARTY_NOTICES.adoc",
]);

/// Files that change how a check itself behaves.
///
/// Editing the toolchain pin, the task graph or a workflow can change the
/// outcome of any check, so those changes are verified in full rather than
/// scoped by what they appear to touch.
const CHECK_DEFINITION_ROOTS = [".github/workflows/", ".cargo/"];
const CHECK_DEFINITION_FILES = new Set([
  "Makefile.toml",
  "flake.lock",
  "flake.nix",
  "rust-toolchain.toml",
]);

const affects = (pathname, roots, files) =>
  startsWithAny(pathname, roots) || files.has(pathname);

/// Decides which quality checks a set of changed paths can actually affect.
///
/// Every job still runs, so the names a branch rule requires keep reporting
/// success. What changes is whether the job does its work or records that the
/// change cannot reach it. A change that alters how a check behaves, and a
/// change this function cannot place, both run everything: being wrong in that
/// direction costs time, while the other direction ships unverified code.
export function qualityScope(paths) {
  const everything = {
    rustSource: true,
    documents: true,
    adapters: true,
    dependencies: true,
    fuzz: true,
    nixPackage: true,
    semver: true,
  };
  if (paths.length === 0) return everything;
  if (
    paths.some((pathname) =>
      affects(pathname, CHECK_DEFINITION_ROOTS, CHECK_DEFINITION_FILES) ||
      !classifyCandidatePath(pathname).classified
    )
  ) {
    return everything;
  }
  const rustSource = paths.some((pathname) =>
    affects(pathname, RUST_SOURCE_ROOTS, RUST_SOURCE_FILES)
  );
  const dependencies = paths.some((pathname) =>
    affects(pathname, DEPENDENCY_AUDIT_ROOTS, DEPENDENCY_AUDIT_FILES)
  );
  return {
    rustSource,
    documents: paths.some((pathname) => affects(pathname, DOCUMENT_ROOTS, DOCUMENT_FILES)),
    adapters: paths.some((pathname) => startsWithAny(pathname, ADAPTER_ROOTS)),
    dependencies,
    // Fuzz targets and the Nix package both build the core crates, so only a
    // change to Rust source or its build inputs can change their outcome.
    fuzz: rustSource,
    nixPackage: rustSource,
    semver: rustSource,
  };
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
    // Only a pull request is scoped. `main` is where independently reviewed
    // branches meet for the first time, so it is verified in full even when
    // each branch was verified on its own.
    quality: pullRequest ? qualityScope(paths) : qualityScope([]),
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
      `quality_rust_source=${plan.quality.rustSource}`,
      `quality_documents=${plan.quality.documents}`,
      `quality_adapters=${plan.quality.adapters}`,
      `quality_dependencies=${plan.quality.dependencies}`,
      `quality_fuzz=${plan.quality.fuzz}`,
      `quality_nix_package=${plan.quality.nixPackage}`,
      `quality_semver=${plan.quality.semver}`,
      "",
    ].join("\n"),
    { flag: "a" },
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();

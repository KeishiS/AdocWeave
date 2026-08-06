import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");
// Quality gates allowed to restore a dependency build. They only verify source
// and distribute nothing, so a restored build cannot reach a release artifact.
// The fuzz gate is deliberately absent: it builds with a nightly toolchain and
// sanitizers, and its exploration is meant to start from the locked closure.
const CACHEABLE_QUALITY_JOBS = new Set(["rust", "adapters"]);

export function isBuildCache(uses) {
  return uses.startsWith("actions/cache/") ||
    uses.startsWith("actions/cache@") ||
    uses.startsWith("Swatinem/rust-cache@");
}

function fail(message) {
  throw new Error(message);
}

function requireText(source, value, message) {
  if (!source.includes(value)) fail(message);
}

function requireCommand(source, value, message) {
  if (typeof source !== "string") fail(message);
  const executable = source.split("\n")
    .map((line) => line.replace(/\s+#.*$/, ""))
    .join("\n");
  requireText(executable, value, message);
}

function requireExactCommand(source, expected, message) {
  if (typeof source !== "string") fail(message);
  const actual = source.split("\n")
    .map((line) => line.replace(/\s+#.*$/, ""))
    .join(" ")
    .trim()
    .replace(/\s+/g, " ");
  if (actual !== expected) fail(message);
}

function parseWorkflow(name, source) {
  const directory = mkdtempSync(join(tmpdir(), "adocweave-workflow-policy-"));
  const path = join(directory, "workflow.yml");
  writeFileSync(path, source);
  const parsed = spawnSync("yq", ["-o=json", ".", path], { encoding: "utf8" });
  rmSync(directory, { force: true, recursive: true });
  if (parsed.status !== 0) {
    fail(`cannot parse workflow ${name}: ${parsed.stderr.trim() || parsed.error?.message}`);
  }
  return JSON.parse(parsed.stdout);
}

function workflowUses(document) {
  const uses = [];
  function visit(value, path) {
    if (Array.isArray(value)) {
      value.forEach((item, index) => visit(item, `${path}[${index}]`));
      return;
    }
    if (!value || typeof value !== "object") return;
    for (const [key, child] of Object.entries(value)) {
      const location = path ? `${path}.${key}` : key;
      if (key === "uses" && typeof child === "string") uses.push({ location, value: child });
      else visit(child, location);
    }
  }
  visit(document, "");
  return uses;
}

function step(job, predicate, message) {
  const found = (job?.steps ?? []).find(predicate);
  if (!found) fail(message);
  return found;
}

function requireNeeds(job, expected, message) {
  const actual = typeof job?.needs === "string" ? [job.needs] : job?.needs;
  if (!Array.isArray(actual) || actual.length !== expected.length ||
      expected.some((name) => !actual.includes(name))) fail(message);
}

function requirePermission(document, name, value, message) {
  if (document?.permissions?.[name] !== value) fail(message);
}

function requireTimeout(job, value, message) {
  if (job?.["timeout-minutes"] !== value) fail(message);
}

// The release manifest is the only place that decides the Node.js version.
// Runners without Nix need setup-node, but writing a version there lets the
// devShell and the workflow drift apart with nothing to notice. The workflow
// may read the manifest and nothing else.
function requireManifestNodeVersion(job) {
  const setup = (job?.steps ?? [])
    .filter((item) => typeof item.uses === "string" && item.uses.startsWith("actions/setup-node@"));
  if (setup.length !== 1) fail("native smoke must configure Node.js exactly once");
  const version = setup[0].with?.["node-version"];
  const resolver = (job?.steps ?? []).find((item) => item.id === "node-version");
  if (!resolver) fail("native smoke must resolve the Node.js version before setup-node");
  requireCommand(
    resolver.run,
    "jq -er .nodeVersion release-manifest.json",
    "the Node.js version must come from the release manifest",
  );
  if (version !== `\${{ steps.${resolver.id}.outputs.value }}`) {
    fail("setup-node must consume the resolved release manifest value, not a literal version");
  }
}

export function parseMakeTasks(source) {
  const headers = [...source.matchAll(/^\[tasks\.([^\]]+)\]\s*$/gm)];
  const tasks = new Map();
  headers.forEach((header, index) => {
    const bodyStart = header.index + header[0].length;
    const bodyEnd = headers[index + 1]?.index ?? source.length;
    const body = source.slice(bodyStart, bodyEnd);
    const alias = body.match(/^alias\s*=\s*"([^"]+)"\s*$/m)?.[1];
    const dependencyBody = body.match(/^dependencies\s*=\s*\[([\s\S]*?)\]/m)?.[1];
    const dependencies = dependencyBody === undefined
      ? undefined
      : [...dependencyBody.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
    const argumentBody = body.match(/^args\s*=\s*\[([\s\S]*?)\]/m)?.[1];
    const args = argumentBody === undefined
      ? undefined
      : [...argumentBody.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
    tasks.set(header[1], { alias, args, dependencies });
  });
  return tasks;
}

export function validateCompatibilityProbeWorkflow({
  makefile,
  protectedWorkflows,
  source,
}) {
  const document = parseWorkflow("textlint-plugin-compatibility-probe.yml", source);
  const triggers = Object.keys(document.on ?? {}).sort();
  if (JSON.stringify(triggers) !== JSON.stringify(["schedule", "workflow_dispatch"])) {
    fail("textlint compatibility probe must only support schedule and workflow_dispatch");
  }
  if (JSON.stringify(document.on.schedule) !== JSON.stringify([{ cron: "17 6 * * 1" }])) {
    fail("textlint compatibility probe must use the reviewed weekly schedule");
  }
  if (JSON.stringify(document.permissions) !== JSON.stringify({ contents: "read" })) {
    fail("textlint compatibility probe must only read repository contents");
  }
  if (document.concurrency?.group !== "textlint-plugin-compatibility-probe" ||
      document.concurrency?.["cancel-in-progress"] !== false) {
    fail("textlint compatibility probe must serialize observations without cancelling a running probe");
  }
  const jobNames = Object.keys(document.jobs ?? {});
  if (JSON.stringify(jobNames) !== JSON.stringify(["observe-latest-resolution"])) {
    fail("textlint compatibility workflow must contain only the observation job");
  }
  const job = document.jobs["observe-latest-resolution"];
  requireTimeout(job, 30, "textlint compatibility observation must have a timeout");
  if (job.needs !== undefined || job.if !== undefined || job["continue-on-error"] !== undefined) {
    fail("textlint compatibility observation must remain an independent scheduled signal");
  }
  const checkout = step(
    job,
    (item) => item.uses?.startsWith("actions/checkout@"),
    "textlint compatibility probe checkout is missing",
  );
  if (checkout.with?.["persist-credentials"] !== false) {
    fail("textlint compatibility probe must not persist checkout credentials");
  }
  step(
    job,
    (item) => item.uses?.startsWith("DeterminateSystems/determinate-nix-action@"),
    "textlint compatibility probe must use the locked Nix environment",
  );
  requireExactCommand(
    step(
      job,
      (item) => item.name ===
        "Build, verify, and observe the latest compatible dependency resolution",
      "textlint compatibility observation step is missing",
    ).run,
    "nix develop .#ci -c cargo make textlint-plugin-compatibility-probe",
    "textlint compatibility workflow must build, verify, and probe the current checkout",
  );
  if (source.includes("secrets.") || /\bgh\s|permissions:\s*write/m.test(source)) {
    fail("textlint compatibility probe must not receive secrets or mutate GitHub state");
  }
  for (const [name, protectedSource] of Object.entries(protectedWorkflows)) {
    if (protectedSource.includes("textlint-plugin-compatibility-probe")) {
      fail(`textlint compatibility probe must not enter the ${name} workflow`);
    }
  }
  const tasks = parseMakeTasks(makefile);
  requireTask(tasks, "textlint-plugin-compatibility-probe", {
    dependencies: ["textlint-plugin-package-contract"],
  });
}

function requireTask(tasks, name, expected) {
  const actual = tasks.get(name);
  if (!actual) fail(`Makefile is missing task: ${name}`);
  if (Object.hasOwn(expected, "alias") && actual.alias !== expected.alias) {
    fail(`${name} must alias ${expected.alias}`);
  }
  if (Object.hasOwn(expected, "dependencies") &&
      JSON.stringify(actual.dependencies) !== JSON.stringify(expected.dependencies)) {
    fail(`${name} dependencies must exactly match the canonical gate`);
  }
  if (Object.hasOwn(expected, "args") && JSON.stringify(actual.args) !== JSON.stringify(expected.args)) {
    fail(`${name} arguments must exactly match the canonical gate`);
  }
}

export function validatePinnedActions(workflows) {
  for (const [name, source] of Object.entries(workflows)) {
    const document = parseWorkflow(name, source);
    for (const reference of workflowUses(document)) {
      if (reference.value.startsWith("./")) continue;
      if (!/@[0-9a-f]{40}$/.test(reference.value)) {
        fail(`${name} ${reference.location} uses an action that is not pinned to a full commit SHA: ${reference.value}`);
      }
    }
  }
}

export function installationE2ESchedule({ nativeRequired, verifyCandidateResult }) {
  return nativeRequired === true && verifyCandidateResult === "success" ? "run" : "skipped";
}

function requireBrowserStartupInteger(source, name, expected) {
  if (typeof source !== "string") fail("browser startup policy source is missing");
  const matches = [
    ...source.matchAll(
      new RegExp(`^export const ${name} = ([0-9][0-9_]*);$`, "gm"),
    ),
  ];
  if (matches.length !== 1) {
    fail(`browser startup policy must export ${name} exactly once`);
  }
  const actual = Number(matches[0][1].replaceAll("_", ""));
  if (actual !== expected) {
    fail(`browser startup policy ${name} must equal ${expected}`);
  }
}

export function validateBrowserStartupPolicy(source) {
  requireBrowserStartupInteger(source, "BROWSER_STARTUP_ATTEMPTS", 3);
  requireBrowserStartupInteger(source, "BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS", 20_000);
  requireBrowserStartupInteger(source, "BROWSER_STARTUP_TOTAL_TIMEOUT_MS", 75_000);
}

export function validateReleaseWorkflowPolicy({
  release,
  dispatch,
  publish,
  contract,
  smoke,
  dist,
  makefile,
  plan,
  windowsDistBootstrap,
  windowsDistInstaller,
  browserStartup,
  textlintPackageContract,
  compatibilityProbe,
}) {
  validateCompatibilityProbeWorkflow({
    makefile,
    protectedWorkflows: { contract, publish, release, smoke },
    source: compatibilityProbe,
  });
  validateBrowserStartupPolicy(browserStartup);
  const releaseDoc = parseWorkflow("release.yml", release);
  const dispatchDoc = parseWorkflow("release-dispatch.yml", dispatch);
  const publishDoc = parseWorkflow("release-publish.yml", publish);
  const contractDoc = parseWorkflow("quality.yml", contract);
  const smokeDoc = parseWorkflow("native-artifact-smoke.yml", smoke);
  const releaseJobs = releaseDoc.jobs ?? {};
  const dispatchJobs = dispatchDoc.jobs ?? {};
  const contractJobs = contractDoc.jobs ?? {};
  const publishJob = publishDoc.jobs?.publish;

  if (!Object.hasOwn(releaseDoc.on ?? {}, "pull_request") ||
      !releaseDoc.on?.push?.branches?.includes("main")) {
    fail("release workflow must exercise pull requests and every main push");
  }
  if (releaseDoc.on.push.tags !== undefined ||
      Object.hasOwn(releaseDoc.on ?? {}, "workflow_dispatch")) {
    fail("source workflow must not create or publish stable tags");
  }
  requirePermission(releaseDoc, "actions", "read", "release workflow must only read Actions");
  requirePermission(releaseDoc, "contents", "read", "release workflow must only read repository contents");
  if (releaseDoc.concurrency?.group !== "ci-release-${{ github.ref }}") {
    fail("CI and release runs must be serialized per ref");
  }
  if (releaseDoc.concurrency?.["cancel-in-progress"] !== true) {
    fail("superseded pull request and main runs must be cancelled");
  }
  if (releaseJobs.quality?.uses !== "./.github/workflows/quality.yml" ||
      releaseJobs.quality?.if !==
      "github.event_name == 'pull_request' || github.ref == 'refs/heads/main'") {
    fail("pull requests and main pushes must pass quality while tags reuse main quality");
  }
  requireNeeds(releaseJobs.quality, ["changes"], "quality must consume the tested change plan");
  if (releaseJobs.quality?.with?.common_preflight_scheduled !==
      "${{ needs.changes.outputs.preflight_required == 'true' }}") {
    fail("quality must skip only the common preflight that candidate CI schedules separately");
  }

  const changes = releaseJobs.changes;
  const changeRun = step(changes, (item) => item.id === "changes", "fast change planner is missing").run;
  requireCommand(changeRun, 'git diff --name-only "$BASE_SHA" "$GITHUB_SHA"', "pull request planning must inspect the complete base diff");
  requireCommand(changeRun, 'git diff --name-only "$BEFORE_SHA" "$GITHUB_SHA"', "main planning must inspect only the pushed change");
  requireCommand(changeRun, 'git show-ref --verify --quiet "refs/tags/v$version"', "release candidate detection must compare the manifest version with existing stable tags");
  requireCommand(changeRun, "node tools/native-change-plan.mjs", "candidate planning must use the tested local planner");
  if ((changes.steps ?? []).some((item) =>
    item.uses?.startsWith("DeterminateSystems/determinate-nix-action@"))) {
    fail("fast change planning must not wait for Nix installation");
  }

  const preflight = releaseJobs.preflight;
  if (preflight?.if !== "needs.changes.outputs.preflight_required == 'true'") {
    fail("candidate preflight must use the explicit change plan");
  }
  requireNeeds(preflight, ["changes"], "candidate preflight must follow fast planning");
  const preflightRun = (preflight?.steps ?? []).map((item) => item.run).filter(Boolean).join("\n");
  requireCommand(
    preflightRun,
    "nix develop .#ci -c cargo make ci-preflight",
    "candidate preflight must use its canonical local task",
  );
  if (preflight?.["continue-on-error"] !== undefined &&
      preflight["continue-on-error"] !== false) {
    fail("candidate preflight job must not continue after failure");
  }
  const preflightStep = step(preflight, (item) =>
    item.name === "Candidate common preflight", "candidate preflight step is missing");
  if (preflightStep["continue-on-error"] !== undefined &&
      preflightStep["continue-on-error"] !== false) {
    fail("candidate preflight step must not continue after failure");
  }

  const releasePlan = releaseJobs["release-plan"];
  if (releasePlan?.if !== "needs.changes.outputs.preflight_required == 'true'") {
    fail("dist planning must be skipped when no candidate or tag requires it");
  }
  requireNeeds(releasePlan, ["changes", "preflight"], "dist planning must follow candidate preflight");
  step(releasePlan, (item) =>
    item.uses?.startsWith("DeterminateSystems/determinate-nix-action@"),
  "dist planning must use the locked Nix environment");
  const planRun = step(releasePlan, (item) => item.id === "plan", "release plan step is missing").run;
  requireCommand(planRun, 'tools/run-pinned-dist.sh plan --tag="$CANDIDATE_TAG"', "every dist plan must use the locked cargo-dist closure");
  if ((releasePlan.steps ?? []).some((item) =>
    item.run?.includes("git/refs") || item.run?.includes("/releases"))) {
    fail("source release planning must remain read-only");
  }

  for (const [jobName, condition] of [
    ["build-native", "needs.changes.outputs.native_required == 'true'"],
    ["native-smoke", "needs.changes.outputs.native_required == 'true'"],
    ["build-global", "needs.changes.outputs.global_required == 'true'"],
    ["verify-candidate", "always() && needs.changes.outputs.candidate_required == 'true'"],
  ]) {
    if (releaseJobs[jobName]?.if !== condition) {
      fail(`${jobName} must use the explicit candidate change plan`);
    }
  }
  if (releaseJobs["installation-e2e"]?.if !==
      "always() && needs.changes.outputs.native_required == 'true' && needs.verify-candidate.result == 'success'") {
    fail("installation-e2e must run only for a verified native candidate without inheriting unrelated skips");
  }
  for (const [label, matrix, expected] of [
    ["native build", releaseJobs["build-native"]?.strategy?.matrix, "${{ fromJSON(needs.changes.outputs.native_matrix) }}"],
    ["installation E2E", releaseJobs["installation-e2e"]?.strategy?.matrix, "${{ fromJSON(needs.changes.outputs.native_matrix) }}"],
    ["native smoke", smokeDoc.jobs?.smoke?.strategy?.matrix, "${{ fromJSON(inputs.matrix) }}"],
  ]) {
    if (matrix !== expected) fail(`${label} must consume the fast planned matrix`);
  }
  requireNeeds(releaseJobs["build-native"], ["changes", "preflight"], "native build must follow candidate preflight");
  requireNeeds(releaseJobs["build-global"], ["changes", "preflight"], "global build must follow candidate preflight");
  requireNeeds(releaseJobs["native-smoke"], ["changes", "build-native"], "native smoke must consume native builds");
  requireNeeds(releaseJobs["verify-candidate"], ["changes", "native-smoke", "build-global"], "partial candidate dependency edge is incomplete");
  requireNeeds(releaseJobs["installation-e2e"], ["changes", "verify-candidate"], "installation must consume a verified native candidate");
  const completeInstallation = step(
    releaseJobs["installation-e2e"],
    (item) => item.name === "Candidate installation and complete removal",
    "complete candidate installation E2E step is missing",
  );
  requireExactCommand(
    completeInstallation.run,
    'node tools/release-installation-e2e.mjs artifacts "${{ matrix.target }}"',
    "complete candidate installation E2E must retain the complete default scope",
  );
  const pullRequestInstallation = step(
    releaseJobs["installation-e2e"],
    (item) => item.name === "Pull request installation and complete removal",
    "pull request installation E2E step is missing",
  );
  requireExactCommand(
    pullRequestInstallation.run,
    'node tools/release-installation-e2e.mjs artifacts "${{ matrix.target }}" release-manifest.json "native-only"',
    "pull request installation E2E must consume the selected candidate families",
  );
  const globalInstallation = releaseJobs["global-installation-e2e"];
  if (globalInstallation?.if !==
      "always() && github.event_name == 'pull_request' && needs.changes.outputs.global_required == 'true' && needs.verify-candidate.result == 'success'") {
    fail("global installation E2E must run only for a verified selected global candidate");
  }
  requireNeeds(
    globalInstallation,
    ["changes", "verify-candidate"],
    "global installation must consume a verified global candidate",
  );
  requireExactCommand(
    step(
      globalInstallation,
      (item) => item.name === "Global installation and complete removal",
      "global installation E2E step is missing",
    ).run,
    'node tools/release-installation-e2e.mjs artifacts x86_64-unknown-linux-musl release-manifest.json "global-only"',
    "global installation E2E must use the global-only scope",
  );
  const textlintInstallation = releaseJobs["textlint-plugin-installation-e2e"];
  if (textlintInstallation?.if !==
      "always() && needs.changes.outputs.global_required == 'true' && needs.verify-candidate.result == 'success'") {
    fail("textlint plugin installation E2E must run for every verified selected global candidate");
  }
  requireNeeds(
    textlintInstallation,
    ["changes", "verify-candidate"],
    "textlint plugin installation must consume a verified global candidate",
  );
  const textlintMatrix = textlintInstallation?.strategy?.matrix?.include;
  if (JSON.stringify(textlintMatrix) !== JSON.stringify(textlintPackageContract?.e2eMatrix)) {
    fail("textlint plugin installation E2E must cover the Node.js boundary and all supported operating systems");
  }
  const textlintRun = step(
    textlintInstallation,
    (item) => item.name === "Fixed textlint consumer installation and read-only CLI verification",
    "textlint plugin installation E2E step is missing",
  ).run;
  requireCommand(
    textlintRun,
    "node tools/textlint-plugin-consumer-e2e.mjs",
    "textlint plugin E2E must exercise the packed release artifact through the fixed consumer",
  );
  const textlintNpxRun = step(
    textlintInstallation,
    (item) => item.name === "Candidate one-shot npx verification",
    "textlint plugin candidate npx step is missing",
  ).run;
  requireCommand(
    textlintNpxRun,
    "node tools/textlint-plugin-npx-smoke.mjs",
    "textlint plugin candidate must exercise the local tarball through one-shot npx",
  );
  requireManifestNodeVersion(textlintInstallation);

  const postReleaseTextlint = dispatchJobs["textlint-plugin-post-release-smoke"];

  const mergeGate = releaseJobs["merge-gate"];
  if (mergeGate?.name !== "quality / verify" ||
      mergeGate?.if !== "always() && github.event_name == 'pull_request'") {
    fail("the stable quality / verify context must be the final pull request gate");
  }
  requireNeeds(
    mergeGate,
    [
      "changes",
      "quality",
      "preflight",
      "release-plan",
      "build-native",
      "native-smoke",
      "build-global",
      "verify-candidate",
      "installation-e2e",
      "global-installation-e2e",
      "textlint-plugin-installation-e2e",
    ],
    "the final pull request gate must wait for quality and every selected candidate stage",
  );
  const mergeGateRun = step(mergeGate, (item) =>
    item.name === "Required pull request result aggregation",
  "final pull request aggregation step is missing").run;
  for (const [command, message] of [
    ['test "$QUALITY_RESULT" = success', "final gate must require source quality"],
    ['test "$PREFLIGHT_RESULT" = success', "final gate must require candidate preflight"],
    ['test "$RELEASE_PLAN_RESULT" = success', "final gate must require candidate planning"],
    ['test "$BUILD_GLOBAL_RESULT" = success', "final gate must require selected global build"],
    ['test "$GLOBAL_INSTALLATION_RESULT" = success', "final gate must require selected global installation E2E"],
    ['test "$TEXTLINT_INSTALLATION_RESULT" = success', "final gate must require selected textlint plugin installation E2E"],
    ['test "$BUILD_NATIVE_RESULT" = success', "final gate must require selected native builds"],
    ['test "$NATIVE_SMOKE_RESULT" = success', "final gate must require selected native smoke"],
    ['test "$VERIFY_CANDIDATE_RESULT" = success', "final gate must require candidate verification"],
    ['test "$INSTALLATION_RESULT" = success', "final gate must require selected installation E2E"],
  ]) {
    requireCommand(mergeGateRun, command, message);
  }

  const nativeBuildRun = step(releaseJobs["build-native"], (item) =>
    item.name === "Target archive builds", "native build step is missing").run;
  requireCommand(nativeBuildRun, "nix develop .#ci -c tools/run-pinned-dist.sh build", "native archives must use the locked build closure");
  const darwin = step(releaseJobs["build-native"], (item) =>
    item.name === "Darwin archive portability normalization",
  "Darwin normalization is missing");
  if (darwin.if !== "endsWith(matrix.target, '-apple-darwin')") {
    fail("Darwin normalization must be limited to Darwin targets");
  }
  requireCommand(darwin.run, "tools/normalize-darwin-archives.sh", "Darwin archives must replace Nix store dependencies");
  if (release.includes("rustup target add") ||
      release.includes("cargo-dist-installer") ||
      release.includes("curl | sh")) {
    fail("release builds must not bypass the locked toolchain");
  }

  // adocweave-host has a separate filesystem implementation for every platform
  // that is not Linux. Ubuntu quality jobs never compile it, so the host unit
  // tests run on the Darwin and Windows runners that already exist for native
  // builds. Both must run before the archive they would otherwise certify.
  const nativeSteps = releaseJobs["build-native"]?.steps ?? [];
  const darwinHostTests = step(releaseJobs["build-native"], (item) =>
    item.name === "Darwin host unit tests", "Darwin host unit test step is missing");
  if (darwinHostTests.if !== "endsWith(matrix.target, '-apple-darwin')") {
    fail("Darwin host unit tests must be limited to Darwin targets");
  }
  if (darwinHostTests.shell !== "bash") {
    fail("Darwin host unit tests must use the reviewed Bash shell");
  }
  if (darwinHostTests["continue-on-error"] !== undefined &&
      darwinHostTests["continue-on-error"] !== false) {
    fail("Darwin host unit tests must not continue after failure");
  }
  requireExactCommand(darwinHostTests.run, "nix develop .#ci -c cargo test -p adocweave-host --all-features",
    "Darwin host unit tests must run the host package inside the locked closure");
  if (nativeSteps.indexOf(darwinHostTests) >
      nativeSteps.findIndex((item) => item.name === "Target archive builds")) {
    fail("Darwin host unit tests must run before the archive they certify");
  }

  const windowsHostTests = step(releaseJobs["build-native"], (item) =>
    item.name === "Windows host unit tests", "Windows host unit test step is missing");
  if (windowsHostTests.if !== "matrix.build == 'windows'") {
    fail("Windows host unit tests must be limited to Windows builds");
  }
  if (windowsHostTests.shell !== "pwsh") {
    fail("Windows host unit tests must use the reviewed PowerShell shell");
  }
  if (windowsHostTests["continue-on-error"] !== undefined &&
      windowsHostTests["continue-on-error"] !== false) {
    fail("Windows host unit tests must not continue after failure");
  }
  requireExactCommand(windowsHostTests.run, "cargo test -p adocweave-host --all-features",
    "Windows host unit tests must run the host package");
  const windowsToolchainIndex = nativeSteps.findIndex((item) =>
    item.name === "Fixed Windows Rust and cargo-dist installation");
  if (nativeSteps.indexOf(windowsHostTests) < windowsToolchainIndex) {
    fail("Windows host unit tests must follow the fixed Rust installation");
  }
  if (nativeSteps.indexOf(windowsHostTests) >
      nativeSteps.findIndex((item) => item.name === "Windows target archive builds")) {
    fail("Windows host unit tests must run before the archive they certify");
  }

  const windowsVersions = step(releaseJobs["build-native"], (item) =>
    item.name === "Fixed Windows Rust and cargo-dist installation",
  "fixed Windows toolchain step is missing").run;
  requireCommand(windowsVersions, "release-manifest.json", "Windows Rust must use the release manifest");
  requireCommand(windowsVersions, "distribution-plan.json", "Windows cargo-dist must use the distribution plan");
  requireCommand(windowsVersions, "./tools/install-pinned-cargo-dist.ps1", "Windows cargo-dist must use the reviewed bootstrap");
  requireCommand(windowsVersions, "-DownloadTimeoutSeconds 60", "Windows cargo-dist download must have a timeout");
  requireCommand(windowsVersions, "-ExtractionTimeoutSeconds 30", "Windows cargo-dist extraction must have a timeout");
  requireCommand(windowsVersions, 'Join-Path $distDirectory "dist.exe"', "Windows cargo-dist must be verified before use");
  if (windowsDistBootstrap.version !== plan.distVersion) {
    fail("Windows cargo-dist bootstrap version must match the distribution plan");
  }
  const expectedWindowsDistUrl =
    `https://github.com/axodotdev/cargo-dist/releases/download/v${windowsDistBootstrap.version}/${windowsDistBootstrap.asset}`;
  if (windowsDistBootstrap.url !== expectedWindowsDistUrl) {
    fail("Windows cargo-dist bootstrap URL must match its version and asset");
  }
  if (windowsDistBootstrap.schemaVersion !== 1 ||
      windowsDistBootstrap.version !== "0.32.0" ||
      windowsDistBootstrap.asset !== "cargo-dist-x86_64-pc-windows-msvc.zip" ||
      windowsDistBootstrap.url !==
        "https://github.com/axodotdev/cargo-dist/releases/download/v0.32.0/cargo-dist-x86_64-pc-windows-msvc.zip" ||
      windowsDistBootstrap.sha256 !==
        "26e845cabff12a92911ce960af73a86c8f9b2b2d9072b01dfe5b662acf044fa3" ||
      windowsDistBootstrap.executable !== "dist.exe" ||
      JSON.stringify(windowsDistBootstrap.archiveEntries) !== JSON.stringify([
        "CHANGELOG.md",
        "dist.exe",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "README.md",
      ])) {
    fail("Windows cargo-dist bootstrap must exactly pin the reviewed release asset");
  }
  for (const [value, message] of [
    ["Invoke-WebRequest", "Windows cargo-dist bootstrap must use the bounded direct download"],
    ["-MaximumRedirection 5", "Windows cargo-dist bootstrap must bound redirects"],
    ["-TimeoutSec $DownloadTimeoutSeconds", "Windows cargo-dist bootstrap must bound the download"],
    ["Get-FileHash -LiteralPath $archivePath -Algorithm SHA256", "Windows cargo-dist bootstrap must verify SHA-256"],
    ['$actualHash -cne $config.sha256', "Windows cargo-dist bootstrap must reject a checksum mismatch"],
    ["Compare-Object -CaseSensitive $actualEntries $expectedEntries", "Windows cargo-dist bootstrap must reject extra archive entries"],
    ["GetFileName($entryName) -cne $entryName", "Windows cargo-dist bootstrap must reject archive paths"],
    ['$archive.GetEntry($config.executable)', "Windows cargo-dist bootstrap must select only the expected executable"],
    ["CopyToAsync($output, 81920, $cancellation.Token)", "Windows cargo-dist bootstrap must bound extraction"],
    ["Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force", "Windows cargo-dist bootstrap must clean its temporary directory"],
  ]) {
    requireCommand(windowsDistInstaller, value, message);
  }
  const checksumVerification = windowsDistInstaller.indexOf(
    '$actualHash -cne $config.sha256',
  );
  const archiveOpening = windowsDistInstaller.indexOf(
    "[IO.Compression.ZipFile]::OpenRead($archivePath)",
  );
  const entryValidation = windowsDistInstaller.indexOf(
    "Compare-Object -CaseSensitive $actualEntries $expectedEntries",
  );
  const executableExtraction = windowsDistInstaller.indexOf(
    "$archive.GetEntry($config.executable)",
  );
  if (checksumVerification < 0 || archiveOpening <= checksumVerification ||
      entryValidation <= archiveOpening || executableExtraction <= entryValidation) {
    fail("Windows cargo-dist bootstrap must verify hash and entries before extraction");
  }
  if ((windowsDistInstaller.match(/Invoke-WebRequest/g) ?? []).length !== 1 ||
      /Invoke-Expression|\biex\b|Start-Process|cargo\s+install/.test(windowsDistInstaller)) {
    fail("Windows cargo-dist bootstrap must not execute an installer or registry build");
  }
  if (release.includes("cargo install cargo-dist") ||
      release.includes("cargo-dist-installer.ps1")) {
    fail("Windows cargo-dist must not use a registry build or network installer");
  }
  if (workflowUses(releaseDoc).some(({ value }) => isBuildCache(value)) ||
      release.includes("target/cargo-dist-bin")) {
    fail("release workflow must not cache executable build tools");
  }
  // Verification jobs may restore dependency builds because nothing they produce
  // is distributed. Jobs that produce candidate or release artifacts must build
  // every byte from the locked closure.
  for (const [name, job] of Object.entries(contractJobs)) {
    const cached = (job?.steps ?? []).some((item) => isBuildCache(item.uses ?? ""));
    if (cached && !CACHEABLE_QUALITY_JOBS.has(name)) {
      fail(`build caching is limited to the listed verification gates: ${name} must not cache builds`);
    }
  }

  const browserAcceptance = step(releaseJobs["build-global"], (item) =>
    item.name === "Browser, textlint, Zed, and VS Code candidate build and runtime verification",
  "global candidate build and browser acceptance is missing");
  if (browserAcceptance.if !== undefined) {
    fail("global candidate build and browser acceptance must always run together");
  }
  if (releaseJobs["build-global"]?.["continue-on-error"] !== undefined &&
      releaseJobs["build-global"]["continue-on-error"] !== false) {
    fail("global candidate job must not continue after a browser acceptance failure");
  }
  if (browserAcceptance["continue-on-error"] !== undefined &&
      browserAcceptance["continue-on-error"] !== false) {
    fail("browser archive acceptance must not continue after failure");
  }
  // The browser this gate drives has to come from the flake like every other
  // tool, not from whatever the runner image ships.
  if (browserAcceptance.run !==
      "nix develop .#ci-browser -c cargo make release-global-candidate") {
    fail("global candidates must use the exact combined archive gate command in the pinned browser shell");
  }
  const globalUpload = step(releaseJobs["build-global"], (item) =>
    item.uses?.startsWith("actions/upload-artifact@"), "global candidate upload is missing");
  requireCommand(
    globalUpload.with?.path,
    "target/distrib/adocweave-textlint-plugin-asciidoc-*.tgz",
    "global candidate upload must include the textlint plugin tarball",
  );
  const smokeRun = step(smokeDoc.jobs?.smoke, (item) =>
    item.name === "Extracted release binary smoke tests", "native smoke is missing").run;
  requireCommand(smokeRun, "node tools/native-release-smoke.mjs", "native smoke must inspect extracted artifacts");
  if (smoke.includes("npm ci") || smoke.includes("npm test")) {
    fail("native smoke must not repeat source adapter tests");
  }
  requireManifestNodeVersion(smokeDoc.jobs?.smoke);

  const aggregateRun = step(releaseJobs["verify-candidate"], (item) =>
    item.name === "Complete candidate metadata generation and verification",
  "complete candidate metadata step is missing");
  if (aggregateRun.if !== "needs.changes.outputs.release_main == 'true'") {
    fail("complete metadata may only be generated for a release-intent main commit");
  }
  requireCommand(aggregateRun.run, "release-metadata.mjs generate", "release main must generate candidate metadata");
  requireCommand(aggregateRun.run, "release-metadata.mjs verify", "release main must verify candidate metadata");
  const partialRun = step(releaseJobs["verify-candidate"], (item) =>
    item.name === "Pull request candidate verification", "partial candidate verification is missing").run;
  requireCommand(partialRun, "needs.changes.outputs.native_required", "partial verification must receive native selection");
  requireCommand(partialRun, "needs.changes.outputs.global_required", "partial verification must receive global selection");
  const completeInstall = step(releaseJobs["installation-e2e"], (item) =>
    item.name === "Candidate installation and complete removal",
  "release candidate installation is missing");
  if (completeInstall.if !== "needs.changes.outputs.release_main == 'true'") {
    fail("complete installation may only run for release-intent main");
  }
  const nixInstall = step(releaseJobs["installation-e2e"], (item) =>
    item.name === "Nix package build and execution", "Nix package acceptance is missing").run;
  for (const output of ["public-contract", "package-smoke", "nixos-package-evaluation"]) {
    requireCommand(nixInstall, output, `candidate must verify Nix ${output}`);
  }

  if (JSON.stringify(Object.keys(dispatchDoc.on ?? {})) !== JSON.stringify(["workflow_dispatch"])) {
    fail("stable release must only support reviewed workflow_dispatch requests");
  }
  const dispatchInputs = dispatchDoc.on?.workflow_dispatch?.inputs;
  for (const name of ["candidate_sha", "finalization_pr"]) {
    if (dispatchInputs?.[name]?.required !== true || dispatchInputs[name].type !== "string") {
      fail(`stable release input ${name} must be a required string`);
    }
  }
  if (dispatchDoc.concurrency?.group !== "stable-release" ||
      dispatchDoc.concurrency?.["cancel-in-progress"] !== false) {
    fail("stable release requests must be serialized without cancellation");
  }
  for (const [name, value] of [["actions", "read"], ["contents", "read"], ["pull-requests", "read"]]) {
    requirePermission(dispatchDoc, name, value, `stable release must grant only ${name}: ${value}`);
  }
  const dispatchPermissions = Object.fromEntries(Object.entries(dispatchDoc.permissions ?? {}).sort());
  if (JSON.stringify(dispatchPermissions) !== JSON.stringify({
    actions: "read",
    contents: "read",
    "pull-requests": "read",
  })) {
    fail("stable release top-level permissions must be exactly the reviewed read-only set");
  }
  for (const [name, job] of Object.entries(dispatchJobs)) {
    if (name !== "publish" && job.permissions !== undefined) {
      fail(`stable release pre-publication job ${name} must inherit read-only permissions`);
    }
  }
  if (dispatch.includes("secrets.")) {
    fail("stable release dispatcher must not access secrets before the isolated publisher");
  }
  const readiness = dispatchJobs.readiness;
  if (readiness?.if !== "github.ref == 'refs/heads/main'") {
    fail("release readiness must run only from the default branch workflow");
  }
  const readinessCheckout = step(readiness, (item) => item.uses?.startsWith("actions/checkout@"),
    "release readiness checkout is missing");
  if (readinessCheckout.with?.ref !== "${{ github.sha }}" ||
      readinessCheckout.with?.["fetch-depth"] !== 0 ||
      readinessCheckout.with?.["fetch-tags"] !== true ||
      readinessCheckout.with?.["persist-credentials"] !== false) {
    fail("release readiness must execute trusted default-branch code with complete history and no credentials");
  }
  const readinessStep = step(readiness, (item) => item.id === "readiness",
    "reviewed final candidate readiness step is missing");
  requireExactCommand(readinessStep.run, "node tools/release-readiness.mjs",
    "release readiness must use the tested helper");
  const readinessEnvironment = Object.fromEntries(Object.entries(readinessStep.env ?? {}).sort());
  if (JSON.stringify(readinessEnvironment) !== JSON.stringify({
    CANDIDATE_SHA: "${{ inputs.candidate_sha }}",
    DISPATCH_SHA: "${{ github.sha }}",
    FINALIZATION_PR: "${{ inputs.finalization_pr }}",
    GH_TOKEN: "${{ github.token }}",
  })) {
    fail("release readiness must bind candidate input to the trusted dispatch SHA evidence");
  }
  requireNeeds(dispatchJobs.plan, ["readiness"], "publish planning must follow readiness");
  requireNeeds(dispatchJobs["reuse-candidate"], ["readiness", "plan"],
    "candidate reuse must consume readiness and the immutable plan");
  const reusedDownload = step(dispatchJobs["reuse-candidate"], (item) =>
    item.uses?.startsWith("actions/download-artifact@"), "candidate download is missing");
  if (reusedDownload.with?.name !== "release-candidate" ||
      reusedDownload.with?.["github-token"] !== "${{ github.token }}" ||
      reusedDownload.with?.repository !== "${{ github.repository }}" ||
      reusedDownload.with?.["run-id"] !== "${{ needs.readiness.outputs.candidate_run_id }}") {
    fail("publication must download only the readiness-selected main candidate");
  }
  const reuseRun = step(dispatchJobs["reuse-candidate"], (item) =>
    item.name === "Reused candidate verification", "candidate verification is missing").run;
  requireCommand(reuseRun, 'release-metadata.mjs verify artifacts "$CANDIDATE_SHA"',
    "publication must verify reused metadata against the frozen SHA");
  requireNeeds(dispatchJobs.publish, ["readiness", "plan", "reuse-candidate"],
    "publication must consume readiness, plan, and verified candidate");
  if (dispatchJobs.publish?.uses !== "./.github/workflows/release-publish.yml" ||
      dispatchJobs.publish?.with?.candidate_sha !== "${{ needs.readiness.outputs.candidate_sha }}" ||
      dispatchJobs.publish?.with?.tag !== "${{ needs.readiness.outputs.tag }}" ||
      dispatchJobs.publish?.with?.plan !== "${{ needs.plan.outputs.manifest }}") {
    fail("publisher must receive only readiness-selected immutable inputs");
  }
  requireNeeds(postReleaseTextlint, ["readiness", "publish"],
    "post-release smoke must follow the selected publication");
  const postReleaseRun = step(postReleaseTextlint,
    (item) => item.name === "Published textlint asset, checksum, and one-shot npx observation",
    "published textlint observation step is missing").run;
  requireExactCommand(postReleaseRun,
    "nix develop .#ci -c cargo make textlint-plugin-post-release-npx-smoke",
    "post-release smoke must use the real GitHub Release URL");

  // The fuzz gate uses its own shell so that the nightly toolchain cargo-fuzz
  // needs stays out of every other job's closure.
  for (const [name, task, timeout, shell] of [
    ["rust", "quality-rust-source", 25, ".#ci"],
    ["adapters", "quality-adapters", 25, ".#ci"],
    ["dependencies", "dependency-governance", 15, ".#ci"],
    ["fuzz", "fuzz", 15, ".#ci-fuzz"],
    ["nix-package", "nix-package-check", 20, ".#ci"],
    ["semver", "semver-check", 20, ".#ci-semver"],
  ]) {
    requireTimeout(contractJobs[name], timeout, `${name} quality job must have a timeout`);
    const run = (contractJobs[name]?.steps ?? []).map((item) => item.run).filter(Boolean).join("\n");
    requireCommand(run, `nix develop ${shell} -c cargo make ${task}`, `${name} must use its canonical local task`);
  }
  // Scoping a gate must not lose one of its halves: the rust job runs the
  // document checks as well, under its own condition.
  requireCommand(
    (contractJobs.rust?.steps ?? []).map((item) => item.run).filter(Boolean).join("\n"),
    "nix develop .#ci -c cargo make quality-documents",
    "the rust quality job must also run the document gate",
  );
  const completeFast = step(contractJobs["source-fast"], (item) =>
    item.name === "Complete fast source policy execution",
  "complete fast source step is missing");
  if (completeFast.if !== "${{ !inputs.common_preflight_scheduled }}" ||
      completeFast.run !== "nix develop .#ci -c cargo make quality-fast") {
    fail("non-candidate source-fast must retain the complete canonical gate");
  }
  const remainingFast = step(contractJobs["source-fast"], (item) =>
    item.name === "Fast source policy execution after candidate preflight",
  "post-preflight fast source step is missing");
  if (remainingFast.if !== "${{ inputs.common_preflight_scheduled }}" ||
      remainingFast.run !== "nix develop .#ci -c cargo make quality-fast-after-preflight") {
    fail("candidate source-fast must avoid repeating the common preflight");
  }
  requireTimeout(contractJobs["source-fast"], 10, "source-fast quality job must have a timeout");
  requireNeeds(
    contractJobs.aggregate,
    ["source-fast", "rust", "adapters", "dependencies", "fuzz", "nix-package", "semver"],
    "the stable required check must aggregate every local gate unit",
  );
  if (contractJobs.aggregate?.if !== "always()") {
    fail("the reusable quality aggregate must report failures reliably");
  }
  requireTimeout(contractJobs.aggregate, 5, "reusable quality aggregate must have a timeout");
  const qualityAggregate = step(contractJobs.aggregate, (item) =>
    item.name === "Required quality result aggregation",
  "reusable quality result aggregation step is missing");
  if (qualityAggregate.env?.SEMVER_RESULT !== "${{ needs.semver.result }}") {
    fail("reusable quality aggregate must receive the semver result");
  }
  requireCommand(
    qualityAggregate.run,
    'test "$SEMVER_RESULT" = success',
    "reusable quality aggregate must require semver success",
  );
  if (contractJobs.verify !== undefined) {
    fail("reusable quality must reserve the quality / verify context for the final candidate gate");
  }
  if (contractJobs["nix-package"]?.if !== "inputs.run_nix_package") {
    fail("Nix package validation must be controlled by an explicit caller input");
  }
  if (contract.includes("github.event_name") || contract.includes("github.ref")) {
    fail("the reusable quality workflow must not infer its caller event or tag");
  }

  const tasks = parseMakeTasks(makefile);
  requireTask(tasks, "ci-preflight", {
    dependencies: [
      "fmt-check",
      "protocol-generated-check",
      "release-contract",
      "workflow-lint",
      "candidate-path-audit",
    ],
  });
  requireTask(tasks, "quality-fast", {
    dependencies: ["ci-preflight", "quality-fast-after-preflight"],
  });
  requireTask(tasks, "quality-fast-after-preflight", {
    dependencies: [
      "platform-contract",
      "docs-check",
      "adoc-check-targets",
    ],
  });
  requireTask(tasks, "platform-contract", {
    args: [
      "--test",
      "tools/native-lsp-smoke.test.mjs",
      "tools/platform-contract.test.mjs",
      "tools/release-installation-e2e.test.mjs",
      "tools/textlint-plugin-npx-smoke.test.mjs",
      "tools/textlint-plugin-compatibility-probe.test.mjs",
      "tools/textlint-plugin-post-release-smoke.test.mjs",
      "tools/textlint-plugin-release-smoke.test.mjs",
      "tools/textlint-plugin-e2e/installed-tree.test.mjs",
      "tools/verify-textlint-plugin-reproducibility.test.mjs",
      "tools/native-change-plan.test.mjs",
      "tools/verify-native-pr-candidate.test.mjs",
      "tools/config-schema.test.mjs",
    ],
  });
  // The gate is split so a change that cannot reach Rust source still runs the
  // document checks, and the other way round. Running `quality-rust` keeps
  // performing both.
  requireTask(tasks, "quality-rust", {
    dependencies: ["quality-rust-source", "quality-documents"],
  });
  requireTask(tasks, "quality-rust-source", {
    dependencies: ["check", "cross-native-check", "clippy", "test", "doc-check"],
  });
  requireTask(tasks, "quality-documents", {
    dependencies: ["adoc-check", "docs-lint", "docs-prose-lint", "html5-check"],
  });
  requireTask(tasks, "quality-adapters", {
    dependencies: [
      "check-wasm",
      "check-zed",
      "check-zed-wasm",
      "check-vscode",
      "clippy-zed",
      "test-web-worker",
      "protocol-check",
      "test-zed",
      "test-vscode",
      "test-vscode-extension-host",
      "test-vscode-release-package",
      "test-cross-runtime",
      "textlint-plugin-check",
    ],
  });
  requireTask(tasks, "textlint-plugin-check", {
    dependencies: [
      "textlint-plugin-public-js-unit",
      "textlint-plugin-wasm-contract",
      "textlint-plugin-browser-isolation",
      "textlint-repository-prose-lint",
    ],
  });
  requireTask(tasks, "textlint-plugin-package-contract-unit", {
    args: [
      "--test",
      "packages/textlint-plugin-asciidoc/package-contract.test.mjs",
      "packages/textlint-plugin-asciidoc/package-stage.test.mjs",
      "packages/textlint-plugin-asciidoc/package-archive.test.mjs",
    ],
  });
  requireTask(tasks, "textlint-plugin-package-contract", {
    dependencies: [
      "textlint-plugin-package-contract-unit",
      "package-textlint-plugin-release",
    ],
  });
  requireTask(tasks, "textlint-plugin-release-consumer-e2e", {
    dependencies: ["textlint-plugin-package-contract"],
  });
  requireTask(tasks, "textlint-plugin-candidate-npx-smoke", {
    dependencies: ["textlint-plugin-package-contract"],
  });
  requireTask(tasks, "textlint-plugin-reproducibility", {});
  requireTask(tasks, "browser-runtime-check", {
    dependencies: ["test-browser-smoke", "test-browser-bundler"],
  });
  requireTask(tasks, "release-global-candidate", {
    dependencies: [
      "release-global-artifacts",
      "browser-runtime-check",
      "textlint-plugin-release-consumer-e2e",
      "textlint-plugin-candidate-npx-smoke",
    ],
  });
  requireTask(tasks, "release-check", {
    dependencies: [
      "ci",
      "semver-check",
      "test-profile-release",
      "wasm-size",
      "release-global-candidate",
      "release-installation-e2e-host",
      "dist-plan",
    ],
  });
  requireTask(tasks, "release-gate", { alias: "release-check" });
  requireTask(tasks, "quality", {
    dependencies: ["quality-fast", "quality-rust", "quality-adapters"],
  });
  requireTask(tasks, "verify", {
    dependencies: ["quality", "dependency-governance", "fuzz", "nix-package-check"],
  });
  requireTask(tasks, "ci", { alias: "verify" });
  if (tasks.get("ci").dependencies !== undefined) {
    fail("ci alias must not define a second dependency graph");
  }

  if (JSON.stringify(releaseDoc).includes('"secrets"') || release.includes("secrets.")) {
    fail("build jobs must not receive repository secrets");
  }
  if (/gh release\s+(create|upload|edit|delete)/.test(release)) {
    fail("read-only workflows must not mutate GitHub Releases");
  }
  for (const [permission, value] of [["attestations", "write"], ["contents", "read"], ["id-token", "write"]]) {
    requirePermission(publishDoc, permission, value, `publisher is missing ${permission}: ${value}`);
    if (dispatchJobs.publish?.permissions?.[permission] !== value) {
      fail(`publisher caller is missing ${permission}: ${value}`);
    }
  }
  if (publishJob?.environment !== "github-release") {
    fail("publisher must use the named github-release environment");
  }
  requireTimeout(publishJob, 20, "publisher must have a timeout");
  const publishRuns = (publishJob?.steps ?? []).map((item) => item.run).filter(Boolean).join("\n");
  for (const [value, message] of [
    ["node tools/release-notes.mjs", "publisher must validate release notes"],
    ['node tools/release-readiness.mjs --assert-tag-absent "$tag"', "publisher must fail closed when rechecking the stable tag"],
    ["release already exists", "publisher must reject replacement"],
    ['gh api --method POST "repos/$GITHUB_REPOSITORY/releases"', "publisher must create a draft"],
    ["-F draft=true", "publisher must stage a private draft"],
    ['upload_url="$(jq -r', "publisher must use the draft upload URL returned by GitHub"],
    ['"$upload_url?name=$name"', "publisher must upload assets through the returned draft URL"],
    ["gh api --method PATCH", "publisher must address the verified draft"],
    ["-F draft=false", "publication must be the final mutation"],
    ["gh api --method DELETE", "failed publication must remove its draft"],
  ]) requireCommand(publishRuns, value, message);
  const attestation = step(publishJob, (item) =>
    item.uses?.startsWith("actions/attest@") && item.with?.["subject-path"] === "artifacts/*",
  "the complete public asset set must be attested");
  const publication = step(publishJob, (item) =>
    typeof item.run === "string" && item.run.includes("-F draft=false"),
  "release publication step is missing");
  if (publishJob.steps.indexOf(attestation) > publishJob.steps.indexOf(publication)) {
    fail("public assets must be attested before publication");
  }
  const publisherToken = step(publishJob, (item) =>
    item.uses?.startsWith("actions/create-github-app-token@"),
  "publisher must obtain a scoped GitHub App token");
  const publisherTokenInputs = Object.fromEntries(
    Object.entries(publisherToken.with ?? {}).sort(),
  );
  if (JSON.stringify(publisherTokenInputs) !== JSON.stringify({
    "app-id": "${{ vars.RELEASE_PUBLISHER_APP_ID }}",
    "permission-contents": "write",
    "private-key": "${{ secrets.RELEASE_PUBLISHER_PRIVATE_KEY }}",
  })) {
    fail("publisher App token must request only contents: write with credentials from the github-release environment");
  }
  const tagCreation = step(publishJob, (item) => item.name === "Immutable stable tag creation",
    "publisher must create the frozen stable tag");
  requireCommand(tagCreation.run, 'gh api --method POST "repos/$GITHUB_REPOSITORY/git/tags"',
    "publisher must create an annotated tag object");
  requireCommand(tagCreation.run, 'gh api --method POST "repos/$GITHUB_REPOSITORY/git/refs"',
    "publisher must create the stable tag ref");
  if (publishJob.steps.indexOf(attestation) > publishJob.steps.indexOf(tagCreation)) {
    fail("all release inputs must be attested before stable tag creation");
  }
  const cleanup = step(publishJob, (item) => item.if === "failure()",
    "failed publication must clean up its draft");
  const cleanupEnvironment = Object.fromEntries(Object.entries(cleanup.env ?? {}).sort());
  if (JSON.stringify(cleanupEnvironment) !== JSON.stringify({
    GH_TOKEN: "${{ steps.publisher-token.outputs.token }}",
    RELEASE_ID: "${{ steps.draft.outputs.release-id }}",
  })) {
    fail("publisher cleanup must receive only its token and own draft ID");
  }
  requireExactCommand(cleanup.run,
    'if [ -z "$RELEASE_ID" ]; then echo "draft release ID is unavailable; cleanup is not attempted" exit 0 fi ' +
      'draft="$(gh api "repos/$GITHUB_REPOSITORY/releases/$RELEASE_ID" 2>/dev/null || true)" ' +
      'if [ "$(jq -r \'.draft // false\' <<<"${draft:-{}}")" = true ]; then ' +
      'gh api --method DELETE "repos/$GITHUB_REPOSITORY/releases/$RELEASE_ID" fi',
    "publisher cleanup must only inspect and delete its own known draft ID");
  if (publishRuns.includes("/releases/tags/") ||
      /gh release\s+(upload|view|edit)/.test(publishRuns)) {
    fail("private drafts must never use the tag-only release API");
  }
  const secretReferences = [...publish.matchAll(/secrets\.([A-Z0-9_]+)/g)].map((match) => match[1]);
  if (JSON.stringify([...new Set(secretReferences)]) !==
      JSON.stringify(["RELEASE_PUBLISHER_PRIVATE_KEY"])) {
    fail("publisher must receive only the dedicated GitHub App private key");
  }

  requireTimeout(smokeDoc.jobs?.smoke, 10, "native smoke must have a timeout");
  requireTimeout(releaseJobs["installation-e2e"], 15, "installation must have a timeout");
  requireTimeout(textlintInstallation, 15, "textlint plugin installation must have a timeout");
  requireTimeout(postReleaseTextlint, 15, "textlint post-release smoke must have a timeout");
  requireTimeout(dispatchJobs["reuse-candidate"], 15, "candidate reuse must have a timeout");
  requireText(dist, 'cargo-dist-version = "0.32.0"', "cargo-dist must be pinned exactly");
  requireText(dist, 'allow-dirty = ["ci"]', "workflow override must be intentional");
  requireText(dist, 'hosting = "github"', "GitHub Releases must be the only configured host");
}

export function loadWorkflowPolicyInputs() {
  const directory = new URL("../.github/workflows/", import.meta.url);
  const workflows = Object.fromEntries(readdirSync(directory)
    .filter((name) => name.endsWith(".yml"))
    .map((name) => [name, read(`.github/workflows/${name}`)]));
  return {
    workflows,
    release: workflows["release.yml"],
    dispatch: workflows["release-dispatch.yml"],
    publish: workflows["release-publish.yml"],
    contract: workflows["quality.yml"],
    smoke: workflows["native-artifact-smoke.yml"],
    dist: read("dist-workspace.toml"),
    makefile: read("Makefile.toml"),
    plan: JSON.parse(read("release/distribution-plan.json")),
    windowsDistBootstrap: JSON.parse(read("release/windows-dist-bootstrap.json")),
    windowsDistInstaller: read("tools/install-pinned-cargo-dist.ps1"),
    browserStartup: read("tools/browser-startup.mjs"),
    compatibilityProbe: workflows["textlint-plugin-compatibility-probe.yml"],
    textlintPackageContract: loadTextlintPluginPackageContract(),
  };
}

export function main() {
  const inputs = loadWorkflowPolicyInputs();
  validatePinnedActions(inputs.workflows);
  validateReleaseWorkflowPolicy(inputs);
  process.stdout.write("release workflow policy verified\n");
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}

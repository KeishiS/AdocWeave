import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");

function fail(message) {
  throw new Error(message);
}

function requireText(source, text, message) {
  if (!source.includes(text)) fail(message);
}

function requireCommand(source, text, message) {
  if (typeof source !== "string") fail(message);
  const executable = source.split("\n")
    .map((line) => line.replace(/\s+#.*$/, ""))
    .join("\n");
  requireText(executable, text, message);
}

function parseWorkflow(name, source) {
  const directory = mkdtempSync(join(tmpdir(), "adocweave-workflow-policy-"));
  const path = join(directory, "workflow.yml");
  writeFileSync(path, source);
  const parsed = spawnSync("yq", ["-o=json", ".", path], { encoding: "utf8" });
  rmSync(directory, { force: true, recursive: true });
  if (parsed.status !== 0) {
    const detail = parsed.stderr.trim() || parsed.error?.message || `exit status ${parsed.status}`;
    fail(`cannot parse workflow ${name}: ${detail}`);
  }
  try {
    return JSON.parse(parsed.stdout);
  } catch (error) {
    fail(`yq returned invalid JSON for ${name}: ${error.message}`);
  }
}

function entries(value) {
  return value && typeof value === "object" ? Object.entries(value) : [];
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
      if (key === "uses" && typeof child === "string") {
        uses.push({ location, value: child });
      } else {
        visit(child, location);
      }
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
  if (!Array.isArray(actual) || actual.length !== expected.length
    || expected.some((name) => !actual.includes(name))) fail(message);
}

function requirePermission(document, name, value, message) {
  if (document?.permissions?.[name] !== value) fail(message);
}

function requireTimeout(job, value, message) {
  if (job?.["timeout-minutes"] !== value) fail(message);
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

export function validateReleaseWorkflowPolicy({ release, publish, contract, smoke, dist }) {
  const releaseDoc = parseWorkflow("release.yml", release);
  const publishDoc = parseWorkflow("release-publish.yml", publish);
  const contractDoc = parseWorkflow("quality.yml", contract);
  const smokeDoc = parseWorkflow("native-artifact-smoke.yml", smoke);

  const releaseJobs = releaseDoc.jobs ?? {};
  const publishJob = publishDoc.jobs?.publish;
  const contractJobs = contractDoc.jobs ?? {};

  if (!Object.hasOwn(releaseDoc.on ?? {}, "pull_request") || !releaseDoc.on?.push) {
    fail("release workflow must exercise pull requests and pushes");
  }
  if (!releaseDoc.on.push.branches?.includes("main")) {
    fail("release workflow must validate every main push before tagging");
  }
  requirePermission(releaseDoc, "contents", "read", "release build workflow must be read-only");
  if (releaseDoc.concurrency?.group !== "ci-release-${{ github.ref }}") {
    fail("CI and release runs must be serialized per ref");
  }
  if (releaseDoc.concurrency?.["cancel-in-progress"] !== "${{ github.event_name == 'pull_request' }}") {
    fail("only superseded pull request runs may be cancelled");
  }

  if (releaseJobs.quality?.uses !== "./.github/workflows/quality.yml") {
    fail("every event must pass the reusable quality gate");
  }
  const pushOnly = Object.entries(releaseJobs)
    .filter(([, job]) => job.if === "github.event_name == 'push'")
    .map(([name]) => name).sort();
  const expectedPushOnly = ["build-global", "build-native", "installation-e2e", "native-smoke", "plan", "verify-candidate"];
  if (JSON.stringify(pushOnly) !== JSON.stringify(expectedPushOnly)) {
    fail("the plan and exactly five candidate jobs must be limited to main and tag pushes");
  }

  requireNeeds(releaseJobs["build-native"], ["plan", "quality"], "native builds must depend on plan and quality");
  requireNeeds(releaseJobs["build-global"], ["plan", "quality"], "global builds must depend on plan and quality");
  requireNeeds(releaseJobs["verify-candidate"], ["plan", "native-smoke", "build-global"], "candidate verification dependency edge is incomplete");
  requireNeeds(releaseJobs["installation-e2e"], ["verify-candidate"], "installation E2E must consume only a verified candidate");
  requireNeeds(releaseJobs.publish, ["plan", "verify-candidate", "installation-e2e"], "publication must depend on candidate installation acceptance");
  if (releaseJobs.publish?.if !== "needs.plan.outputs.publishing == 'true'") {
    fail("pull requests must not invoke publication");
  }
  if (releaseJobs.publish?.uses !== "./.github/workflows/release-publish.yml") {
    fail("publication must be isolated in its reusable workflow");
  }

  const planRun = step(releaseJobs.plan, (item) => item.id === "plan", "release plan step is missing").run;
  requireCommand(planRun, 'candidate_tag="v$(jq -r .packageVersion release-manifest.json)"', "non-tag candidate plans must use the release train version");
  requireCommand(planRun, 'tools/run-pinned-dist.sh plan --tag="$candidate_tag"', "every dist plan must use the locked cargo-dist closure");
  const tagRun = step(releaseJobs.plan, (item) => item.name === "Verify publication tag is the current main commit", "publication tag check is missing").run;
  requireCommand(tagRun, 'test "$(git rev-parse refs/remotes/origin/main)" = "$GITHUB_SHA"', "publication tags must identify the current main commit");

  for (const jobName of ["plan", "build-native"]) {
    step(releaseJobs[jobName], (item) => item.uses?.startsWith("DeterminateSystems/determinate-nix-action@"), `${jobName} must install the locked Nix environment`);
  }
  const nativeBuildRun = step(releaseJobs["build-native"], (item) => item.name === "Build target archives", "native build step is missing").run;
  requireCommand(nativeBuildRun, "tools/run-pinned-dist.sh build", "native archives must use the locked cargo-dist closure");
  if (release.includes("cargo-dist-installer") || release.includes("curl | sh")) {
    fail("release workflow must not execute a network-fetched cargo-dist installer");
  }

  const aggregateRun = step(releaseJobs["verify-candidate"], (item) => item.name === "Generate and verify metadata for the complete candidate", "candidate metadata step is missing").run;
  requireCommand(aggregateRun, "node tools/release-metadata.mjs generate artifacts", "metadata must be generated from the aggregated candidate");
  requireCommand(aggregateRun, "node tools/release-metadata.mjs verify artifacts", "the aggregate job must verify exact release metadata");
  const globalStep = step(releaseJobs["build-global"], (item) => item.name === "Build and verify browser and Zed archives", "global artifact step is missing");
  const globalRun = globalStep.run;
  requireCommand(globalRun, "nix develop .#ci -c cargo make release-global-artifacts", "uploaded browser and Zed archives must pass their complete artifact gate");
  if (globalStep.env?.ADOCWEAVE_BROWSER !== "chromium") fail("global artifact browser must come from the GitHub runner image");
  const installRun = step(releaseJobs["installation-e2e"], (item) => item.name === "Install and completely remove the candidate", "installation E2E step is missing").run;
  requireCommand(installRun, "node tools/release-installation-e2e.mjs artifacts", "both Linux architectures must run the installation lifecycle");

  const uploads = (releaseJobs["verify-candidate"]?.steps ?? []).filter((item) => item.uses?.startsWith("actions/upload-artifact@"));
  if (!uploads.some((item) => item.with?.name === "release-candidate" && item.with?.["retention-days"] === 14)) {
    fail("verified candidates must have bounded retention");
  }
  if (!Object.values(releaseJobs).flatMap((job) => job.steps ?? [])
    .some((item) => item.uses?.startsWith("actions/upload-artifact@") && item.with?.["retention-days"] === 7)) {
    fail("intermediate build artifacts must have short retention");
  }

  requireTimeout(contractJobs.verify, 30, "the complete quality gate must have a timeout");
  requireTimeout(contractJobs.dependencies, 15, "dependency governance must have a timeout");
  requireTimeout(smokeDoc.jobs?.smoke, 10, "native smoke tests must have a timeout");
  requireTimeout(publishJob, 20, "publication must have a timeout and cleanup path");
  const qualityStep = step(contractJobs.verify, (item) => item.name === "Run the complete quality gate", "complete quality step is missing");
  const qualityRun = qualityStep.run;
  requireCommand(qualityRun, "nix develop .#ci -c cargo make release-gate", "the reusable quality workflow must run the canonical local gate");
  if (qualityStep.env?.ADOCWEAVE_BROWSER !== "chromium") fail("quality browser must come from the GitHub runner image");
  const dependencyRun = step(contractJobs.dependencies, (item) => item.name === "Audit dependency boundaries", "dependency governance step is missing").run;
  requireCommand(dependencyRun, "nix develop .#ci -c cargo make dependency-governance", "quality must audit every dependency boundary");
  const msrvRun = step(contractJobs.msrv, (item) => item.name === "Install and verify the declared minimum Rust version", "MSRV step is missing").run;
  requireCommand(msrvRun, ".rust_version] | unique", "CI must derive one MSRV from workspace package metadata");
  requireCommand(msrvRun, 'cargo "+$msrv" check --locked --workspace --all-targets --all-features', "CI must enforce the declared workspace MSRV");
  requireCommand(msrvRun, 'test "$zed_msrv" = "$msrv"', "Zed and workspace MSRV declarations must match");
  requireCommand(msrvRun, 'cargo "+$msrv" check --manifest-path editors/zed/Cargo.toml --locked --all-targets', "CI must enforce the declared Zed MSRV");
  const tagStep = step(contractJobs.verify, (item) => item.name === "Verify an optional publication tag", "optional publication tag step is missing");
  if (tagStep.if !== "inputs.release_tag != ''") fail("only explicit publication tags may receive tag validation");
  if (contract.includes("github.event_name") || contract.includes("github.ref")) {
    fail("the reusable quality workflow must not infer its caller event or publication tag");
  }

  if (JSON.stringify(releaseDoc).includes('"secrets"') || release.includes("secrets.")) {
    fail("build and aggregate jobs must not receive repository secrets");
  }
  if (/gh release\s+(create|upload|edit|delete)/.test(release)) {
    fail("the read-only workflow must not mutate GitHub Releases");
  }

  for (const permission of ["attestations", "contents", "id-token"]) {
    requirePermission(publishDoc, permission, "write", `publisher is missing permission: ${permission}: write`);
    if (releaseJobs.publish?.permissions?.[permission] !== "write") {
      fail(`publisher caller is missing permission: ${permission}: write`);
    }
  }
  if (publishJob?.environment !== "github-release") {
    fail("publisher must use the protected github-release environment");
  }
  const publishRuns = (publishJob?.steps ?? []).map((item) => item.run).filter(Boolean).join("\n");
  for (const [text, message] of [
    ["node tools/release-notes.mjs", "publication must append and validate the required release notes"],
    ["release already exists", "publisher must reject release replacement"],
    ['gh api --method POST "repos/$GITHUB_REPOSITORY/releases"', "publisher must create a draft only after verification"],
    ["-F draft=true", "publisher must stage assets in a private draft"],
    ['upload_url="$(jq -r', "publisher must use the upload URL returned with the private draft"],
    ["gh api --method PATCH", "publication must address the verified draft by release ID"],
    ["-F draft=false", "publication must be the final mutation"],
    ["gh api --method DELETE", "failed publication must delete an incomplete draft by release ID"],
  ]) requireCommand(publishRuns, text, message);
  step(publishJob, (item) => item.uses?.startsWith("actions/attest@") && item.with?.["subject-path"] === "artifacts/*", "the complete public asset set must be attested");
  step(publishJob, (item) => item.if === "failure()", "failed publication must clean up its draft");
  if (publishRuns.includes("/releases/tags/") || /gh release\s+(upload|view|edit)/.test(publishRuns)) {
    fail("private drafts must never be looked up through the tag-only release API");
  }
  if (JSON.stringify(publishDoc).includes('"secrets"') || publish.includes("secrets.")) {
    fail("publisher must use the scoped GitHub token rather than repository secrets");
  }

  requireText(dist, 'cargo-dist-version = "0.32.0"', "cargo-dist must be pinned exactly");
  requireText(dist, 'allow-dirty = ["ci"]', "repository-owned workflow must be declared as an intentional dist override");
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
    publish: workflows["release-publish.yml"],
    contract: workflows["quality.yml"],
    smoke: workflows["native-artifact-smoke.yml"],
    dist: read("dist-workspace.toml"),
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

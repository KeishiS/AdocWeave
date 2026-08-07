import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

// This policy intentionally checks only what other gates cannot: supply-chain
// pinning, write-permission scope, and direct secret access. Workflow syntax
// is actionlint's job (the `workflow-lint` task), and everything a workflow
// *does* is verified by running it, not by re-stating its script text here.

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");

// The only places allowed to hold a write permission. Publication needs
// `contents: write` for the GitHub Release and `id-token: write` for artifact
// attestation; every other workflow and job stays read-only.
const ALLOWED_WRITE_GRANTS = new Set([
  "release-publish.yml top-level",
  "release-dispatch.yml job publish",
]);

function fail(message) {
  throw new Error(message);
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

function* workflowUses(document) {
  for (const [jobName, job] of Object.entries(document.jobs ?? {})) {
    if (typeof job.uses === "string") yield { location: `job ${jobName}`, value: job.uses };
    for (const step of job.steps ?? []) {
      if (typeof step.uses === "string") yield { location: `job ${jobName}`, value: step.uses };
    }
  }
}

export function validatePinnedActions(workflows) {
  for (const [name, document] of Object.entries(workflows)) {
    for (const { location, value } of workflowUses(document)) {
      if (value.startsWith("./")) continue;
      if (!/@[0-9a-f]{40}$/.test(value)) {
        fail(`${name} ${location} uses an action that is not pinned to a full commit SHA: ${value}`);
      }
    }
  }
}

export function validateWritePermissionGrants(workflows) {
  const grants = (permissions, location) => {
    for (const [scope, level] of Object.entries(permissions ?? {})) {
      if (level !== "read" && level !== "none") {
        if (!ALLOWED_WRITE_GRANTS.has(location)) {
          fail(`${location} grants ${scope}: ${level}; write permissions are reserved for publication`);
        }
      }
    }
  };
  for (const [name, document] of Object.entries(workflows)) {
    if (document.permissions === undefined) {
      fail(`${name} must declare explicit top-level permissions`);
    }
    grants(document.permissions, `${name} top-level`);
    for (const [jobName, job] of Object.entries(document.jobs ?? {})) {
      grants(job.permissions, `${name} job ${jobName}`);
    }
  }
}

export function validateNoDirectSecretAccess(sources) {
  for (const [name, source] of Object.entries(sources)) {
    if (source.includes("secrets.")) {
      fail(`${name} reads from the secrets context; workflows use the ambient job token only`);
    }
  }
}

export function loadWorkflowPolicyInputs() {
  const sources = {};
  for (const file of readdirSync(new URL(".github/workflows/", ROOT))) {
    if (file.endsWith(".yml")) sources[file] = read(`.github/workflows/${file}`);
  }
  return {
    sources,
    workflows: Object.fromEntries(
      Object.entries(sources).map(([name, source]) => [name, parseWorkflow(name, source)]),
    ),
  };
}

export function validateReleaseWorkflowPolicy({ sources, workflows }) {
  validatePinnedActions(workflows);
  validateWritePermissionGrants(workflows);
  validateNoDirectSecretAccess(sources);
}

export function main() {
  validateReleaseWorkflowPolicy(loadWorkflowPolicyInputs());
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

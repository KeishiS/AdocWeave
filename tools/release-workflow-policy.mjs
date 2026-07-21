import { readFileSync, readdirSync } from "node:fs";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");

function fail(message) {
  throw new Error(message);
}

function requireText(source, text, message) {
  if (!source.includes(text)) fail(message);
}

export function validatePinnedActions(workflows) {
  for (const [name, source] of Object.entries(workflows)) {
    for (const match of source.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)/gm)) {
      const reference = match[1];
      if (reference.startsWith("./")) continue;
      if (!/@[0-9a-f]{40}$/.test(reference)) {
        fail(`${name} uses an action that is not pinned to a full commit SHA: ${reference}`);
      }
    }
  }
}

export function validateReleaseWorkflowPolicy({ release, publish, contract, smoke, dist }) {
  validatePinnedActions({ release, publish, contract, smoke });

  requireText(release, "pull_request:", "release workflow must exercise the plan on pull requests");
  requireText(release, "contents: read", "release build workflow must be read-only");
  requireText(release, "persist-credentials: false", "checkout credentials must not persist");
  requireText(release, "group: release-${{ github.ref }}", "release runs must be serialized per ref");
  requireText(release, "cancel-in-progress: false", "release runs must not cancel an active publication");
  requireText(release, "needs: [plan, verify-candidate]", "publication must depend on the verified complete candidate");
  requireText(release, "if: needs.plan.outputs.publishing == 'true'", "pull requests must not invoke publication");
  requireText(release, "uses: ./.github/workflows/release-publish.yml", "publication must be isolated in its reusable workflow");
  requireText(release, "node tools/release-metadata.mjs generate artifacts", "metadata must be generated from the aggregated candidate");
  requireText(release, "node tools/release-metadata.mjs verify artifacts", "the aggregate job must verify exact release metadata");
  requireText(release, "bash tools/package-browser-release.sh", "the browser archive must use the tested repository builder directly");
  requireText(release, "test -s \"$archive\"", "the browser archive must be non-empty before upload");
  requireText(release, "tar -tJf \"$archive\"", "the browser archive must be validated before upload");
  requireText(release, "name: release-candidate", "only a verified candidate may cross the publish boundary");
  if (release.includes("secrets:") || release.includes("secrets.")) {
    fail("build and aggregate jobs must not receive repository secrets");
  }
  if (/gh release\s+(create|upload|edit|delete)/.test(release)) {
    fail("the read-only workflow must not mutate GitHub Releases");
  }

  for (const permission of ["attestations: write", "contents: write", "id-token: write"]) {
    requireText(publish, permission, `publisher is missing permission: ${permission}`);
  }
  requireText(publish, "environment: github-release", "publisher must use the protected github-release environment");
  requireText(publish, "release already exists", "publisher must reject release replacement");
  requireText(publish, "gh release create \"$tag\"", "publisher must create a draft only after verification");
  requireText(publish, "--draft", "publisher must stage assets in a private draft");
  requireText(publish, "actions/attest@", "every release must receive GitHub provenance attestations");
  requireText(publish, "subject-path: artifacts/*", "the complete public asset set must be attested");
  requireText(publish, "gh api --method PATCH", "publication must address the verified draft by release ID");
  requireText(publish, "-F draft=false", "publication must be the final mutation");
  requireText(publish, "if: failure()", "failed publication must clean up its draft");
  requireText(publish, "gh api --method DELETE", "failed publication must delete an incomplete draft by release ID");
  if (publish.includes("/releases/tags/") || /gh release\s+(upload|view|edit)/.test(publish)) {
    fail("private drafts must never be looked up through the tag-only release API");
  }
  if (publish.includes("secrets:") || publish.includes("secrets.")) {
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
    contract: workflows["release-contract.yml"],
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

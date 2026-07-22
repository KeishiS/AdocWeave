import { readFileSync, readdirSync } from "node:fs";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");

function fail(message) {
  throw new Error(message);
}

function requireText(source, text, message) {
  const executable = source.split("\n").filter((line) => !line.trimStart().startsWith("#")).join("\n");
  if (!executable.includes(text)) fail(message);
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
  requireText(release, "branches:\n      - main", "release workflow must validate every main push before tagging");
  requireText(release, 'candidate_tag="v$(jq -r .packageVersion release-manifest.json)"', "non-tag candidate plans must use the release train version");
  requireText(release, 'dist plan --tag="$candidate_tag"', "every dist plan must select the complete release train explicitly");
  requireText(release, 'test "$(git rev-parse refs/remotes/origin/main)" = "$GITHUB_SHA"', "publication tags must identify the current main commit");
  requireText(release, "contents: read", "release build workflow must be read-only");
  requireText(release, "persist-credentials: false", "checkout credentials must not persist");
  requireText(release, "group: ci-release-${{ github.ref }}", "CI and release runs must be serialized per ref");
  requireText(release, "cancel-in-progress: ${{ github.event_name == 'pull_request' }}", "only superseded pull request runs may be cancelled");
  requireText(release, "uses: ./.github/workflows/quality.yml", "every event must pass the reusable quality gate");
  requireText(release, "if: github.event_name == 'push'", "candidate artifacts must be limited to main and tag pushes");
  const pushOnlyJobs = [...release.matchAll(/^    if: github\.event_name == 'push'$/gm)].length;
  if (pushOnlyJobs !== 6) fail("the plan and exactly five candidate jobs must be limited to main and tag pushes");
  requireText(release, "needs: [plan, verify-candidate, installation-e2e]", "publication must depend on candidate installation acceptance");
  requireText(release, "if: needs.plan.outputs.publishing == 'true'", "pull requests must not invoke publication");
  requireText(release, "uses: ./.github/workflows/release-publish.yml", "publication must be isolated in its reusable workflow");
  requireText(release, "node tools/release-metadata.mjs generate artifacts", "metadata must be generated from the aggregated candidate");
  requireText(release, "node tools/release-metadata.mjs verify artifacts", "the aggregate job must verify exact release metadata");
  requireText(release, "nix develop -c cargo make release-global-artifacts", "uploaded browser and Zed archives must pass their complete artifact gate");
  requireText(publish, "node tools/release-notes.mjs", "publication must append and validate the required release notes");
  requireText(release, "name: release-candidate", "only a verified candidate may cross the publish boundary");
  requireText(release, "retention-days: 7", "intermediate build artifacts must have short retention");
  requireText(release, "retention-days: 14", "verified candidates must have bounded retention");
  requireText(contract, "timeout-minutes: 30", "the complete quality gate must have a timeout");
  requireText(smoke, "timeout-minutes: 10", "native smoke tests must have a timeout");
  requireText(publish, "timeout-minutes: 20", "publication must have a timeout and cleanup path");
  requireText(release, "node tools/release-installation-e2e.mjs artifacts", "both Linux architectures must run the installation lifecycle");
  requireText(contract, "nix develop -c cargo make release-gate", "the reusable quality workflow must run the canonical local gate");
  requireText(contract, ".rust_version] | unique", "CI must derive one MSRV from workspace package metadata");
  requireText(contract, 'cargo "+$msrv" check --locked --workspace --all-targets --all-features', "CI must enforce the declared workspace MSRV");
  requireText(contract, "if: inputs.release_tag != ''", "only explicit publication tags may receive tag validation");
  if (contract.includes("github.event_name") || contract.includes("github.ref")) {
    fail("the reusable quality workflow must not infer its caller event or publication tag");
  }
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
  requireText(publish, 'gh api --method POST "repos/$GITHUB_REPOSITORY/releases"', "publisher must create a draft only after verification");
  requireText(publish, "-F draft=true", "publisher must stage assets in a private draft");
  requireText(publish, 'upload_url="$(jq -r', "publisher must use the upload URL returned with the private draft");
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

import assert from "node:assert/strict";
import test from "node:test";

import {
  loadWorkflowPolicyInputs,
  validatePinnedActions,
  validateReleaseWorkflowPolicy,
} from "./release-workflow-policy.mjs";

test("repository release workflows satisfy the least-privilege policy", () => {
  validateReleaseWorkflowPolicy(loadWorkflowPolicyInputs());
});

test("every external action requires a full commit SHA", () => {
  assert.throws(
    () => validatePinnedActions({
      "unsafe.yml": "jobs:\n  unsafe:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v6\n",
    }),
    /not pinned/,
  );
});

test("fork-equivalent build workflows cannot receive secrets or publish", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({ ...inputs, release: `${inputs.release}\nsecrets: inherit\n` }),
    /must not receive repository secrets/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({ ...inputs, release: `${inputs.release}\n# gh release create unsafe\n` }),
    /must not mutate GitHub Releases/,
  );
});

test("publisher cannot omit its protected environment or cleanup", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({ ...inputs, publish: inputs.publish.replace("environment: github-release", "environment: unprotected") }),
    /protected github-release environment/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({ ...inputs, publish: inputs.publish.replace("if: failure()", "if: success()") }),
    /clean up its draft/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      publish: `${inputs.publish.replace("environment: github-release", "environment: unprotected")}\n# environment: github-release\n`,
    }),
    /protected github-release environment/,
  );
});

test("quality and candidate jobs cannot infer or broaden their event scope", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      contract: `${inputs.contract}\nCALLER_EVENT: github.event_name\n`,
    }),
    /must not infer/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace("if: github.ref == 'refs/heads/main'", "if: github.event_name == 'push'"),
    }),
    /candidate jobs must be limited to main pushes/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "if: github.event_name == 'pull_request' || github.ref == 'refs/heads/main'",
        "if: github.event_name != 'pull_request'",
      ),
    }),
    /reuse main quality/,
  );
});

test("required fields cannot move to another job or a same-name step", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "needs: [plan, reuse-candidate]",
        "needs: [plan] # reuse-candidate",
      ),
    }),
    /reused candidate verification/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "tools/run-pinned-dist.sh plan",
        "dist plan # tools/run-pinned-dist.sh plan",
      ),
    }),
    /locked cargo-dist closure/,
  );
});

test("network installer cannot replace the locked cargo-dist closure", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        "tools/run-pinned-dist.sh build",
        "curl https://example.invalid/cargo-dist-installer.sh | sh\n          dist build",
      ),
    }),
    /locked cargo-dist closure|network-fetched/,
  );
});

test("quality cannot omit dependency governance or the source gate", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      contract: inputs.contract.replace("nix develop .#ci -c cargo make dependency-governance", "true # dependency-governance"),
    }),
    /audit every dependency boundary/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      contract: inputs.contract.replace("nix develop .#ci -c cargo make quality", "true # quality"),
    }),
    /source quality gate/,
  );
});

test("candidate acceptance cannot omit the Nix package contract", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        '".#checks.${{ matrix.nix-system }}.package-smoke"',
        '".#checks.${{ matrix.nix-system }}.package"',
      ),
    }),
    /both Linux architectures must build and run the Nix package/,
  );
});

test("tag publication must reuse and verify the selected main candidate", () => {
  const inputs = loadWorkflowPolicyInputs();
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace('run-id: ${{ needs.plan.outputs.candidate_run_id }}', 'run-id: ${{ github.run_id }}'),
    }),
    /download the named candidate/,
  );
  assert.throws(
    () => validateReleaseWorkflowPolicy({
      ...inputs,
      release: inputs.release.replace(
        '      - name: Verify the reused candidate\n        run: node tools/release-metadata.mjs verify artifacts "$GITHUB_SHA"',
        '      - name: Verify the reused candidate\n        run: true',
      ),
    }),
    /verify candidate metadata/,
  );
});

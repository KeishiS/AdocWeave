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
    () => validatePinnedActions({ "unsafe.yml": "steps:\n  - uses: actions/checkout@v6\n" }),
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
    () => validateReleaseWorkflowPolicy({ ...inputs, publish: `${inputs.publish}\n# gh release view v0.1.0\n` }),
    /tag-only release API/,
  );
});

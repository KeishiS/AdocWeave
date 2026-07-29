import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  evaluateController,
  evaluateEligibility,
  validatePolicy,
} from "./dependabot-auto-merge-policy.mjs";

const policy = JSON.parse(
  await readFile(new URL("../.github/dependabot-auto-merge-policy.json", import.meta.url)),
);
const SHA = "1".repeat(40);
const BASE_SHA = "2".repeat(40);

function eligibleInput() {
  return {
    policy: { ...structuredClone(policy), enabled: true },
    pullRequest: {
      actor: "dependabot[bot]",
      author: "dependabot[bot]",
      baseRef: "main",
      baseRepository: "KeishiS/adocweave",
      headRef: "dependabot/cargo/serde-1.2.3",
      headRepository: "KeishiS/adocweave",
      headSha: SHA,
      baseSha: BASE_SHA,
      draft: false,
      mergeable: true,
    },
    metadata: {
      securityAlertLookup: true,
      openSecurityAlerts: 0,
      packageEcosystem: "cargo",
      directory: "/",
      targetBranch: "main",
      dependencyType: "direct:production",
      updateType: "version-update:semver-patch",
      dependencies: ["serde"],
      dependencyGroup: "",
      maintainerChanges: false,
    },
    changedFiles: ["Cargo.toml", "Cargo.lock"],
  };
}

function controllerInput() {
  const input = eligibleInput();
  return {
    ...input,
    currentBaseSha: BASE_SHA,
    workflowRun: {
      event: "pull_request",
      conclusion: "success",
      headSha: SHA,
      baseSha: BASE_SHA,
    },
    securityAlerts: {
      lookupCompleted: true,
      openCount: 0,
    },
    changedFiles: [...input.changedFiles],
    review: {
      changesRequested: false,
      approvedCount: 0,
    },
    eligibilityCheck: {
      name: "dependabot / eligibility",
      headSha: SHA,
      conclusion: "success",
      appSlug: "github-actions",
      appId: input.policy.requiredCheckAppId,
    },
    checks: input.policy.requiredChecks.map((name, index) => ({
      name,
      headSha: SHA,
      conclusion: "success",
      appId: input.policy.requiredCheckAppId,
      completedAt: `2026-01-01T00:00:0${index}Z`,
    })),
  };
}

test("tracked policy starts frozen and excludes high-risk ecosystems", () => {
  assert.equal(validatePolicy(policy).enabled, false);
  assert.equal(validatePolicy(policy).requiresStrictStatusChecks, true);
  assert.deepEqual(
    [...new Set(policy.allowedUpdates.map((entry) => entry.packageEcosystem))].sort(),
    ["cargo", "npm"],
  );
  assert.ok(
    policy.allowedUpdates.every(
      (entry) => entry.updateTypes.length === 1
        && entry.updateTypes[0] === "version-update:semver-patch",
    ),
  );
});

test("the conservative Cargo and npm development patch boundaries are accepted", () => {
  assert.deepEqual(evaluateEligibility(eligibleInput()), { eligible: true, reasons: [] });

  const npm = eligibleInput();
  npm.pullRequest.headRef = "dependabot/npm_and_yarn/editors/vscode/typescript-7.0.3";
  npm.metadata = {
    ...npm.metadata,
    packageEcosystem: "npm",
    directory: "/editors/vscode",
    dependencyType: "direct:development",
    dependencies: ["typescript"],
  };
  npm.changedFiles = [
    "editors/vscode/package.json",
    "editors/vscode/package-lock.json",
  ];
  assert.deepEqual(evaluateEligibility(npm), { eligible: true, reasons: [] });
});

for (const [name, mutate, expected] of [
  ["freeze", (input) => { input.policy.enabled = false; }, "release-freeze"],
  ["actor", (input) => { input.pullRequest.actor = "octocat"; }, "actor"],
  ["author", (input) => { input.pullRequest.author = "octocat"; }, "actor"],
  ["fork", (input) => { input.pullRequest.headRepository = "fork/adocweave"; }, "fork"],
  ["branch", (input) => { input.pullRequest.headRef = "feature/serde"; }, "dependabot-branch"],
  ["base", (input) => { input.pullRequest.baseRef = "release"; }, "base-branch"],
  ["metadata directory", (input) => { input.metadata.directory = "/other"; }, "metadata-boundary-denied"],
  ["metadata target", (input) => { input.metadata.targetBranch = "release"; }, "metadata-target-branch"],
  ["major", (input) => { input.metadata.updateType = "version-update:semver-major"; }, "update-type"],
  ["minor", (input) => { input.metadata.updateType = "version-update:semver-minor"; }, "update-type"],
  ["one open security alert", (input) => {
    input.metadata.openSecurityAlerts = 1;
  }, "open-security-alert-or-lookup"],
  ["more than one page of open security alerts", (input) => {
    input.metadata.openSecurityAlerts = 101;
  }, "open-security-alert-or-lookup"],
  ["missing security alert lookup", (input) => {
    input.metadata.securityAlertLookup = false;
  }, "open-security-alert-or-lookup"],
  ["missing security alert count", (input) => {
    delete input.metadata.openSecurityAlerts;
  }, "open-security-alert-or-lookup"],
  ["invalid security alert count", (input) => {
    input.metadata.openSecurityAlerts = "0";
  }, "open-security-alert-or-lookup"],
  ["runtime npm", (input) => {
    input.metadata.packageEcosystem = "npm";
    input.metadata.directory = "/editors/vscode";
    input.metadata.dependencyType = "direct:production";
    input.changedFiles = ["editors/vscode/package.json"];
  }, "dependency-type"],
  ["group", (input) => { input.metadata.dependencyGroup = "tooling"; }, "dependency-group"],
  ["multiple dependencies", (input) => { input.metadata.dependencies = ["serde", "syn"]; }, "dependency-count-or-name"],
  ["maintainer changes", (input) => { input.metadata.maintainerChanges = true; }, "maintainer-changes"],
  ["source file", (input) => { input.changedFiles.push("crates/adocweave/src/lib.rs"); }, "changed-files"],
  ["workflow file", (input) => { input.changedFiles.push(".github/workflows/release.yml"); }, "changed-files"],
]) {
  test(`eligibility rejects ${name}`, () => {
    const input = eligibleInput();
    mutate(input);
    const result = evaluateEligibility(input);
    assert.equal(result.eligible, false);
    assert.ok(result.reasons.includes(expected), result.reasons.join(","));
  });
}

for (const [name, mutate, expected] of [
  ["failed CI", (input) => { input.workflowRun.conclusion = "failure"; }, "ci-workflow-run"],
  ["wrong CI SHA", (input) => { input.workflowRun.headSha = "3".repeat(40); }, "ci-workflow-run"],
  ["stale CI base", (input) => { input.workflowRun.baseSha = "3".repeat(40); }, "ci-workflow-run"],
  ["controller open security alert", (input) => {
    input.securityAlerts.openCount = 1;
  }, "open-security-alert-or-lookup"],
  ["controller alert lookup missing", (input) => {
    delete input.securityAlerts;
  }, "open-security-alert-or-lookup"],
  ["draft", (input) => { input.pullRequest.draft = true; }, "draft"],
  ["conflict", (input) => { input.pullRequest.mergeable = false; }, "merge-conflict-or-unknown"],
  ["unknown mergeability", (input) => { input.pullRequest.mergeable = null; }, "merge-conflict-or-unknown"],
  ["stale base", (input) => { input.currentBaseSha = "4".repeat(40); }, "stale-base"],
  ["controller workflow change", (input) => {
    input.changedFiles.push(".github/workflows/dependabot-eligibility.yml");
  }, "controller-changed-files"],
  ["controller policy tool change", (input) => {
    input.changedFiles.push("tools/dependabot-auto-merge-policy.mjs");
  }, "controller-changed-files"],
  ["controller changed files missing", (input) => {
    delete input.changedFiles;
  }, "controller-changed-files"],
  ["changes requested", (input) => { input.review.changesRequested = true; }, "review"],
  ["missing approval", (input) => {
    input.policy.requiredApprovals = 1;
    input.review.approvedCount = 0;
  }, "review"],
  ["old eligibility", (input) => { input.eligibilityCheck.headSha = "5".repeat(40); }, "eligibility-attestation"],
  ["neutral eligibility", (input) => { input.eligibilityCheck.conclusion = "neutral"; }, "eligibility-attestation"],
  ["wrong eligibility app", (input) => { input.eligibilityCheck.appSlug = "other"; }, "eligibility-attestation"],
  ["wrong eligibility app ID", (input) => { input.eligibilityCheck.appId = 1; }, "eligibility-attestation"],
  ["failed required check", (input) => { input.checks[0].conclusion = "failure"; }, "required-check:quality / dependencies"],
  ["wrong required check app", (input) => { input.checks[0].appId = 1; }, "required-check:quality / dependencies"],
  ["pending required check", (input) => { input.checks[1].conclusion = null; }, "required-check:quality / fuzz"],
  ["old required check", (input) => { input.checks[2].headSha = "6".repeat(40); }, "required-check:quality / nix-package"],
  ["missing required check", (input) => { input.checks.pop(); }, "required-check:quality / verify"],
]) {
  test(`controller rejects ${name}`, () => {
    const input = controllerInput();
    mutate(input);
    const result = evaluateController(input);
    assert.equal(result.eligible, false);
    assert.ok(result.reasons.includes(expected), result.reasons.join(","));
  });
}

test("controller accepts only the current eligible SHA after every required check", () => {
  assert.deepEqual(evaluateController(controllerInput()), { eligible: true, reasons: [] });
});

test("invalid and broadened policies fail closed", () => {
  for (const mutate of [
    (changed) => { changed.enabled = "yes"; },
    (changed) => { changed.requiresStrictStatusChecks = false; },
    (changed) => { changed.requiredChecks.push(changed.requiredChecks[0]); },
    (changed) => { changed.allowedUpdates[0].updateTypes = ["version-update:semver-minor"]; },
    (changed) => { changed.allowedUpdates.push({
      ...changed.allowedUpdates[0],
      packageEcosystem: "github-actions",
    }); },
  ]) {
    const changed = structuredClone(policy);
    mutate(changed);
    assert.throws(() => validatePolicy(changed));
  }
});

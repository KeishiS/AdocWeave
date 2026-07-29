import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  evaluateController,
  evaluateEligibility,
  evaluateReconciliation,
  evaluateStrictRulesetProtection,
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
      securityUpdate: false,
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
      securityUpdate: false,
    },
    changedFiles: [...input.changedFiles],
    review: {
      changesRequested: false,
      approvedCount: input.policy.requiredApprovals,
    },
    eligibilityCheck: {
      id: 100,
      name: "dependabot / eligibility",
      headSha: SHA,
      conclusion: "success",
      appSlug: "github-actions",
      appId: input.policy.requiredCheckAppId,
    },
    checks: input.policy.requiredChecks.map((name, index) => ({
      id: index + 1,
      name,
      headSha: SHA,
      conclusion: "success",
      appId: input.policy.requiredCheckAppId,
      completedAt: `2026-01-01T00:00:0${index}Z`,
    })),
  };
}

test("tracked policy starts frozen and excludes high-risk ecosystems", () => {
  const validated = validatePolicy(policy);
  assert.equal(validated.enabled, false);
  assert.equal(validated.maximumDependencies, 1);
  assert.equal(validated.allowDependencyGroups, false);
  assert.equal(validated.allowIndirectDependencies, false);
  assert.equal(validated.allowMaintainerChanges, false);
  assert.equal(validated.allowSecurityUpdates, false);
  assert.equal(validated.requiresStrictStatusChecks, true);
  assert.equal(validated.requiredApprovals, 1);
  assert.deepEqual(
    [...new Set(policy.allowedUpdates.map((entry) => entry.packageEcosystem))].sort(),
    ["cargo", "npm"],
  );
  assert.ok(
    policy.allowedUpdates.every(
      (entry) => entry.updateTypes.length === 1
        && entry.updateTypes[0] === "version-update:semver-patch"
        && !entry.dependencyTypes.includes("indirect"),
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
  ["indirect Cargo dependency", (input) => {
    input.metadata.dependencyType = "indirect";
  }, "dependency-type"],
  ["group", (input) => { input.metadata.dependencyGroup = "tooling"; }, "dependency-group"],
  ["multiple dependencies", (input) => { input.metadata.dependencies = ["serde", "syn"]; }, "dependency-count-or-name"],
  ["maintainer changes", (input) => { input.metadata.maintainerChanges = true; }, "maintainer-changes"],
  ["security update", (input) => { input.metadata.securityUpdate = true; }, "security-update"],
  ["unknown security update state", (input) => {
    delete input.metadata.securityUpdate;
  }, "security-update"],
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
  ["controller security update", (input) => {
    input.securityAlerts.securityUpdate = true;
  }, "open-security-alert-or-lookup"],
  ["controller security state missing", (input) => {
    delete input.securityAlerts.securityUpdate;
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
    input.review.approvedCount = 0;
  }, "review"],
  ["old eligibility", (input) => { input.eligibilityCheck.headSha = "5".repeat(40); }, "eligibility-attestation"],
  ["missing eligibility ID", (input) => {
    delete input.eligibilityCheck.id;
  }, "eligibility-attestation"],
  ["neutral eligibility", (input) => { input.eligibilityCheck.conclusion = "neutral"; }, "eligibility-attestation"],
  ["wrong eligibility app", (input) => { input.eligibilityCheck.appSlug = "other"; }, "eligibility-attestation"],
  ["wrong eligibility app ID", (input) => { input.eligibilityCheck.appId = 1; }, "eligibility-attestation"],
  ["failed required check", (input) => { input.checks[0].conclusion = "failure"; }, "required-check:quality / dependencies"],
  ["wrong required check app", (input) => { input.checks[0].appId = 1; }, "required-check:quality / dependencies"],
  ["pending required check", (input) => { input.checks[1].conclusion = null; }, "required-check:quality / fuzz"],
  ["old required check", (input) => { input.checks[2].headSha = "6".repeat(40); }, "required-check:quality / nix-package"],
  ["missing required check", (input) => { input.checks.pop(); }, "required-check:quality / verify"],
  ["malformed check inventory", (input) => {
    input.checks.push({
      ...input.checks[0],
      id: null,
    });
  }, "check-inventory"],
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

test("controller rejects a newer pending check regardless of API order", () => {
  for (const reverse of [false, true]) {
    const input = controllerInput();
    const oldSuccess = input.checks[0];
    const newerPending = {
      ...oldSuccess,
      id: 1000,
      conclusion: null,
      completedAt: null,
    };
    input.checks = reverse
      ? [newerPending, ...input.checks]
      : [...input.checks, newerPending];
    const result = evaluateController(input);
    assert.equal(result.eligible, false);
    assert.ok(
      result.reasons.includes(`required-check:${oldSuccess.name}`),
      result.reasons.join(","),
    );
  }
});

function strictRuleset(include = ["~DEFAULT_BRANCH"], exclude = []) {
  return {
    target: "branch",
    enforcement: "active",
    bypass_actors: [],
    conditions: { ref_name: { include, exclude } },
    rules: [
      {
        type: "required_status_checks",
        parameters: {
          strict_required_status_checks_policy: true,
          required_status_checks: policy.requiredChecks.map((context) => ({
            context,
            integration_id: policy.requiredCheckAppId,
          })),
        },
      },
      {
        type: "pull_request",
        parameters: {
          required_approving_review_count: policy.requiredApprovals,
          dismiss_stale_reviews_on_push: true,
          require_last_push_approval: true,
        },
      },
    ],
  };
}

test("strict ruleset protection is only one prerequisite and does not activate frozen policy", () => {
  assert.equal(policy.enabled, false);
  for (const include of [["~DEFAULT_BRANCH"], ["refs/heads/main"]]) {
    assert.deepEqual(
      evaluateStrictRulesetProtection({
        policy,
        branch: "main",
        rulesets: [strictRuleset(include)],
      }),
      { eligible: true, reasons: [] },
    );
  }
});

test("strict ruleset rejects bypass actors in every applicable ruleset", () => {
  for (const references of [
    { include: ["refs/heads/main"], exclude: [] },
    { include: ["refs/heads/*"], exclude: [] },
    { include: ["refs/heads/main"], exclude: ["refs/heads/release"] },
  ]) {
    const bypassed = strictRuleset(["refs/heads/main"]);
    bypassed.conditions.ref_name = references;
    bypassed.bypass_actors.push({
      actor_id: 5,
      actor_type: "RepositoryRole",
      bypass_mode: "always",
    });
    assert.deepEqual(
      evaluateStrictRulesetProtection({
        policy,
        branch: "main",
        rulesets: [strictRuleset(["~DEFAULT_BRANCH"]), bypassed],
      }),
      { eligible: false, reasons: ["strict-ruleset"] },
    );
  }
});

test("reconciliation fails closed for policy freeze and unsafe alert state", () => {
  const safeAlerts = {
    lookupCompleted: true,
    openCount: 0,
    securityUpdate: false,
  };
  assert.deepEqual(
    evaluateReconciliation({ policy, securityAlerts: safeAlerts }),
    { eligible: false, reasons: ["release-freeze"] },
  );
  const enabledPolicy = structuredClone(policy);
  enabledPolicy.enabled = true;
  assert.deepEqual(
    evaluateReconciliation({
      policy: enabledPolicy,
      securityAlerts: { ...safeAlerts, lookupCompleted: false },
    }),
    { eligible: false, reasons: ["open-security-alert-or-lookup"] },
  );
  assert.deepEqual(
    evaluateReconciliation({ policy: enabledPolicy, securityAlerts: safeAlerts }),
    { eligible: true, reasons: [] },
  );
});

for (const [name, mutate] of [
  ["default branch exclusion", (ruleset) => {
    ruleset.conditions.ref_name.exclude = ["~DEFAULT_BRANCH"];
  }],
  ["main exclusion", (ruleset) => {
    ruleset.conditions.ref_name.exclude = ["refs/heads/main"];
  }],
  ["unrelated exclusion", (ruleset) => {
    ruleset.conditions.ref_name.exclude = ["refs/heads/release"];
  }],
  ["wildcard include", (ruleset) => {
    ruleset.conditions.ref_name.include = ["refs/heads/*"];
  }],
  ["inactive enforcement", (ruleset) => {
    ruleset.enforcement = "disabled";
  }],
  ["GitHub Actions bypass", (ruleset) => {
    ruleset.bypass_actors.push({
      actor_id: policy.requiredCheckAppId,
      actor_type: "Integration",
      bypass_mode: "always",
    });
  }],
  ["repository role bypass", (ruleset) => {
    ruleset.bypass_actors.push({
      actor_id: 5,
      actor_type: "RepositoryRole",
      bypass_mode: "always",
    });
  }],
  ["missing bypass inventory", (ruleset) => {
    delete ruleset.bypass_actors;
  }],
  ["non-strict status checks", (ruleset) => {
    ruleset.rules[0].parameters.strict_required_status_checks_policy = false;
  }],
  ["missing required status check", (ruleset) => {
    ruleset.rules[0].parameters.required_status_checks.pop();
  }],
  ["wrong required status check app", (ruleset) => {
    ruleset.rules[0].parameters.required_status_checks[0].integration_id = 1;
  }],
  ["missing pull request rule", (ruleset) => {
    ruleset.rules.pop();
  }],
  ["no required approval", (ruleset) => {
    ruleset.rules[1].parameters.required_approving_review_count = 0;
  }],
  ["stale reviews remain valid", (ruleset) => {
    ruleset.rules[1].parameters.dismiss_stale_reviews_on_push = false;
  }],
  ["last push approval is not required", (ruleset) => {
    ruleset.rules[1].parameters.require_last_push_approval = false;
  }],
]) {
  test(`strict ruleset rejects ${name}`, () => {
    const ruleset = strictRuleset();
    mutate(ruleset);
    assert.deepEqual(
      evaluateStrictRulesetProtection({ policy, branch: "main", rulesets: [ruleset] }),
      { eligible: false, reasons: ["strict-ruleset"] },
    );
  });
}

test("invalid and broadened policies fail closed", () => {
  for (const mutate of [
    (changed) => { changed.enabled = "yes"; },
    (changed) => { changed.maximumDependencies = 2; },
    (changed) => { changed.allowIndirectDependencies = true; },
    (changed) => { changed.allowSecurityUpdates = true; },
    (changed) => { changed.requiresStrictStatusChecks = false; },
    (changed) => { changed.requiredApprovals = 0; },
    (changed) => { changed.requiredCheckAppId = 1; },
    (changed) => { changed.requiredChecks.push(changed.requiredChecks[0]); },
    (changed) => { changed.requiredChecks[0] = "easy-check"; },
    (changed) => { changed.allowedUpdates[0].dependencyTypes.push("indirect"); },
    (changed) => {
      const npm = changed.allowedUpdates.find(
        (entry) => entry.packageEcosystem === "npm",
      );
      npm.dependencyTypes.push("direct:production");
    },
    (changed) => { changed.allowedUpdates[0].directory = "/other"; },
    (changed) => { changed.allowedUpdates[0].changedFiles.push("src/*"); },
    (changed) => {
      changed.allowedUpdates.push({
        ...changed.allowedUpdates[0],
        directory: "/other",
      });
    },
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

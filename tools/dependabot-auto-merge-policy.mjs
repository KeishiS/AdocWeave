import { readFile } from "node:fs/promises";

const DEPENDABOT_LOGIN = "dependabot[bot]";
const DEPENDABOT_BRANCH = /^dependabot\/[a-z0-9_-]+\/[a-zA-Z0-9._/-]+$/;
const SAFE_TEXT = /^[a-zA-Z0-9@._:/,+-]+$/;
const GITHUB_ACTIONS_APP_ID = 15368;
const REQUIRED_CHECKS = new Set([
  "quality / dependencies",
  "quality / fuzz",
  "quality / nix-package",
  "quality / verify",
]);
const ALLOWED_DEPENDENCY_TYPES = new Map([
  ["cargo", new Set(["direct:production", "direct:development"])],
  ["npm", new Set(["direct:development"])],
]);
const ALLOWED_UPDATE_BOUNDARIES = new Map([
  ["cargo:/", new Set(["Cargo.toml", "Cargo.lock", "crates/*/Cargo.toml"])],
  [
    "cargo:/editors/zed",
    new Set(["editors/zed/Cargo.toml", "editors/zed/Cargo.lock"]),
  ],
  ["cargo:/fuzz", new Set(["fuzz/Cargo.toml", "fuzz/Cargo.lock"])],
  [
    "npm:/editors/vscode",
    new Set(["editors/vscode/package.json", "editors/vscode/package-lock.json"]),
  ],
]);

export function validatePolicy(policy) {
  if (!policy || policy.version !== 1 || typeof policy.enabled !== "boolean") {
    throw new Error("invalid Dependabot auto-merge policy version or state");
  }
  if (policy.baseBranch !== "main" || policy.mergeMethod !== "SQUASH") {
    throw new Error("Dependabot auto-merge must target main with squash");
  }
  if (policy.maximumDependencies !== 1
      || policy.allowDependencyGroups !== false
      || policy.allowIndirectDependencies !== false
      || policy.allowMaintainerChanges !== false
      || policy.allowSecurityUpdates !== false
      || policy.requiresStrictStatusChecks !== true
      || !Number.isSafeInteger(policy.requiredApprovals)
      || policy.requiredApprovals < 1
      || policy.requiredCheckAppId !== GITHUB_ACTIONS_APP_ID) {
    throw new Error("Dependabot auto-merge limits must fail closed");
  }
  if (!uniqueStrings(policy.requiredChecks)
      || policy.requiredChecks.length !== REQUIRED_CHECKS.size
      || policy.requiredChecks.some((name) => !REQUIRED_CHECKS.has(name))) {
    throw new Error("Dependabot required checks must be a non-empty unique list");
  }
  if (!Array.isArray(policy.allowedUpdates) || policy.allowedUpdates.length === 0) {
    throw new Error("Dependabot allowed updates must not be empty");
  }
  const boundaries = new Set();
  for (const update of policy.allowedUpdates) {
    const boundary = `${update.packageEcosystem}:${update.directory}`;
    const allowedFiles = ALLOWED_UPDATE_BOUNDARIES.get(boundary);
    if (boundaries.has(boundary)
        || !allowedFiles
        || !/^\/(?:[a-z0-9._-]+(?:\/[a-z0-9._-]+)*)?$/.test(update.directory)
        || !uniqueStrings(update.dependencyTypes)
        || !uniqueStrings(update.updateTypes)
        || !uniqueStrings(update.changedFiles)
        || update.dependencyTypes.some(
          (value) => !ALLOWED_DEPENDENCY_TYPES.get(update.packageEcosystem)?.has(value),
        )
        || update.updateTypes.some((value) => value !== "version-update:semver-patch")
        || update.changedFiles.some(
          (value) => !safeFilePattern(value) || !allowedFiles.has(value),
        )) {
      throw new Error(`invalid Dependabot update boundary: ${boundary}`);
    }
    boundaries.add(boundary);
  }
  if (policy.allowedUpdates.some(({ packageEcosystem }) => packageEcosystem === "github-actions")) {
    throw new Error("GitHub Actions updates must require manual review");
  }
  return policy;
}

export function evaluateEligibility(input) {
  const reasons = [];
  let policy;
  try {
    policy = validatePolicy(input.policy);
  } catch (error) {
    return decision(false, [`policy-invalid:${error.message}`]);
  }
  if (!policy.enabled) reasons.push("release-freeze");
  validatePullRequest(input.pullRequest, policy, reasons);

  const metadata = input.metadata ?? {};
  if (metadata.securityAlertLookup !== true
      || !Number.isSafeInteger(metadata.openSecurityAlerts)
      || metadata.openSecurityAlerts !== 0) {
    reasons.push("open-security-alert-or-lookup");
  }
  const boundary = policy.allowedUpdates.find(
    (candidate) => candidate.packageEcosystem === metadata.packageEcosystem
      && candidate.directory === metadata.directory,
  );
  if (!boundary) reasons.push("metadata-boundary-denied");
  if (metadata.targetBranch !== policy.baseBranch) reasons.push("metadata-target-branch");
  if (metadata.maintainerChanges !== false) reasons.push("maintainer-changes");
  if (metadata.securityUpdate !== false) reasons.push("security-update");
  if (metadata.dependencyGroup) reasons.push("dependency-group");
  if (!Array.isArray(metadata.dependencies)
      || metadata.dependencies.length === 0
      || metadata.dependencies.length > policy.maximumDependencies
      || metadata.dependencies.some((value) => typeof value !== "string" || !SAFE_TEXT.test(value))) {
    reasons.push("dependency-count-or-name");
  }
  if (boundary) {
    if (!boundary.dependencyTypes.includes(metadata.dependencyType)) {
      reasons.push("dependency-type");
    }
    if (!boundary.updateTypes.includes(metadata.updateType)) reasons.push("update-type");
    if (!Array.isArray(input.changedFiles)
        || input.changedFiles.length === 0
        || input.changedFiles.some((path) => !matchesAny(path, boundary.changedFiles))) {
      reasons.push("changed-files");
    }
  }
  return decision(reasons.length === 0, reasons);
}

function validateSecurityAlerts(securityAlerts, reasons) {
  if (securityAlerts?.lookupCompleted !== true
      || !Number.isSafeInteger(securityAlerts.openCount)
      || securityAlerts.openCount !== 0
      || securityAlerts.securityUpdate !== false) {
    reasons.push("open-security-alert-or-lookup");
  }
}

export function evaluateController(input) {
  const reasons = [];
  let policy;
  try {
    policy = validatePolicy(input.policy);
  } catch (error) {
    return decision(false, [`policy-invalid:${error.message}`]);
  }
  if (!policy.enabled) reasons.push("release-freeze");
  validatePullRequest(input.pullRequest, policy, reasons);
  validateSecurityAlerts(input.securityAlerts, reasons);
  if (!input.workflowRun
      || input.workflowRun.event !== "pull_request"
      || input.workflowRun.conclusion !== "success"
      || input.workflowRun.headSha !== input.pullRequest?.headSha
      || input.workflowRun.baseSha !== input.currentBaseSha) {
    reasons.push("ci-workflow-run");
  }
  if (input.pullRequest?.draft === true) reasons.push("draft");
  if (input.pullRequest?.mergeable !== true) reasons.push("merge-conflict-or-unknown");
  if (input.pullRequest?.baseSha !== input.currentBaseSha) reasons.push("stale-base");
  const allowedChangedFiles = policy.allowedUpdates.flatMap((update) => update.changedFiles);
  if (!Array.isArray(input.changedFiles)
      || input.changedFiles.length === 0
      || input.changedFiles.some((path) => !matchesAny(path, allowedChangedFiles))) {
    reasons.push("controller-changed-files");
  }
  if (input.review?.changesRequested === true
      || !Number.isSafeInteger(input.review?.approvedCount)
      || input.review.approvedCount < policy.requiredApprovals) {
    reasons.push("review");
  }
  const attestation = input.eligibilityCheck;
  if (!attestation
      || attestation.name !== "dependabot / eligibility"
      || attestation.headSha !== input.pullRequest?.headSha
      || attestation.conclusion !== "success"
      || attestation.appSlug !== "github-actions"
      || attestation.appId !== policy.requiredCheckAppId) {
    reasons.push("eligibility-attestation");
  }
  const latestChecks = latestChecksByName(input.checks);
  for (const name of policy.requiredChecks) {
    const check = latestChecks.get(name);
    if (!check
        || check.headSha !== input.pullRequest?.headSha
        || check.conclusion !== "success"
        || check.appId !== policy.requiredCheckAppId) {
      reasons.push(`required-check:${name}`);
    }
  }
  return decision(reasons.length === 0, reasons);
}

export function evaluateReconciliation(input) {
  const reasons = [];
  let policy;
  try {
    policy = validatePolicy(input.policy);
  } catch (error) {
    return decision(false, [`policy-invalid:${error.message}`]);
  }
  if (!policy.enabled) reasons.push("release-freeze");
  validateSecurityAlerts(input.securityAlerts, reasons);
  return decision(reasons.length === 0, reasons);
}

export function evaluateStrictRulesetProtection(input) {
  let policy;
  try {
    policy = validatePolicy(input.policy);
  } catch (error) {
    return decision(false, [`policy-invalid:${error.message}`]);
  }
  if (!Array.isArray(input.rulesets) || input.branch !== policy.baseBranch) {
    return decision(false, ["strict-ruleset"]);
  }
  const activeBranchRulesets = input.rulesets.filter(
    (ruleset) => ruleset?.target === "branch" && ruleset.enforcement === "active",
  );
  const possiblyApplicableRulesets = activeBranchRulesets.filter((ruleset) => {
    const references = ruleset?.conditions?.ref_name;
    if (!Array.isArray(references?.include)
        || !Array.isArray(references.exclude)) {
      return true;
    }
    if (references.exclude.some(
      (pattern) => pattern === "~DEFAULT_BRANCH"
        || pattern === `refs/heads/${policy.baseBranch}`,
    )) {
      return false;
    }
    return references.include.some(
      (pattern) => pattern === "~DEFAULT_BRANCH"
        || pattern === `refs/heads/${policy.baseBranch}`
        || typeof pattern !== "string"
        || !/^refs\/heads\/[a-zA-Z0-9._/-]+$/.test(pattern),
    );
  });
  if (possiblyApplicableRulesets.some(
    (ruleset) => !Array.isArray(ruleset.bypass_actors)
      || ruleset.bypass_actors.length !== 0,
  )) {
    return decision(false, ["strict-ruleset"]);
  }
  const strictRulesets = activeBranchRulesets.filter((ruleset) => {
    const references = ruleset?.conditions?.ref_name;
    return Array.isArray(references?.include)
      && references.include.some(
        (pattern) => pattern === "~DEFAULT_BRANCH"
          || pattern === `refs/heads/${policy.baseBranch}`,
      )
      && Array.isArray(references.exclude)
      && references.exclude.length === 0;
  });
  if (strictRulesets.length === 0) {
    return decision(false, ["strict-ruleset"]);
  }
  const eligible = strictRulesets.some((ruleset) => {
    const statusRule = ruleset?.rules?.find(
      (rule) => rule?.type === "required_status_checks",
    );
    const pullRequestRule = ruleset?.rules?.find(
      (rule) => rule?.type === "pull_request",
    );
    const requiredStatusChecks = statusRule?.parameters?.required_status_checks;
    return Array.isArray(ruleset.rules)
      && statusRule?.parameters?.strict_required_status_checks_policy === true
      && Array.isArray(requiredStatusChecks)
      && policy.requiredChecks.every(
        (name) => requiredStatusChecks.some(
          (check) => check?.context === name
            && check.integration_id === policy.requiredCheckAppId,
        ),
      )
      && pullRequestRule?.parameters?.required_approving_review_count
        >= policy.requiredApprovals
      && pullRequestRule?.parameters?.dismiss_stale_reviews_on_push === true
      && pullRequestRule?.parameters?.require_last_push_approval === true;
  });
  return decision(eligible, eligible ? [] : ["strict-ruleset"]);
}

function validatePullRequest(pullRequest, policy, reasons) {
  if (!pullRequest
      || pullRequest.author !== DEPENDABOT_LOGIN
      || pullRequest.actor !== DEPENDABOT_LOGIN) {
    reasons.push("actor");
  }
  if (pullRequest?.baseRef !== policy.baseBranch) reasons.push("base-branch");
  if (pullRequest?.headRepository !== pullRequest?.baseRepository) reasons.push("fork");
  if (typeof pullRequest?.headRef !== "string"
      || !DEPENDABOT_BRANCH.test(pullRequest.headRef)) {
    reasons.push("dependabot-branch");
  }
  if (typeof pullRequest?.headSha !== "string"
      || !/^[0-9a-f]{40}$/.test(pullRequest.headSha)) {
    reasons.push("head-sha");
  }
}

function latestChecksByName(checks) {
  const result = new Map();
  for (const check of Array.isArray(checks) ? checks : []) {
    const previous = result.get(check.name);
    if (!previous || String(previous.completedAt) < String(check.completedAt)) {
      result.set(check.name, check);
    }
  }
  return result;
}

function matchesAny(path, patterns) {
  return typeof path === "string" && patterns.some((pattern) => {
    const expression = pattern
      .split("*")
      .map((part) => part.replace(/[.+?^${}()|[\]\\]/g, "\\$&"))
      .join("[^/]+");
    return new RegExp(`^${expression}$`).test(path);
  });
}

function safeFilePattern(pattern) {
  return typeof pattern === "string"
    && !pattern.startsWith("/")
    && !pattern.includes("..")
    && /^[a-zA-Z0-9._/*-]+$/.test(pattern);
}

function uniqueStrings(values) {
  return Array.isArray(values)
    && values.length > 0
    && values.every((value) => typeof value === "string" && value.length > 0)
    && new Set(values).size === values.length;
}

function decision(eligible, reasons) {
  return {
    eligible,
    reasons: [...new Set(reasons)].sort(),
  };
}

async function main() {
  const [mode, policyPath, inputPath] = process.argv.slice(2);
  if (!["eligibility", "controller", "reconcile", "strict-rulesets"].includes(mode)
      || !policyPath || !inputPath) {
    throw new Error(
      "usage: dependabot-auto-merge-policy.mjs eligibility|controller|reconcile|strict-rulesets POLICY INPUT",
    );
  }
  const policy = JSON.parse(await readFile(policyPath, "utf8"));
  const input = JSON.parse(await readFile(inputPath, "utf8"));
  const result = mode === "eligibility"
    ? evaluateEligibility({ ...input, policy })
    : mode === "controller"
      ? evaluateController({ ...input, policy })
      : mode === "reconcile"
        ? evaluateReconciliation({ ...input, policy })
        : evaluateStrictRulesetProtection({ ...input, policy });
  process.stdout.write(`${JSON.stringify(result)}\n`);
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  await main();
}

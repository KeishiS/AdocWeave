import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import test from "node:test";

const eligibility = await readFile(
  new URL("../.github/workflows/dependabot-eligibility.yml", import.meta.url),
  "utf8",
);
const controller = await readFile(
  new URL("../.github/workflows/dependabot-auto-merge.yml", import.meta.url),
  "utf8",
);
const reviewSignal = await readFile(
  new URL("../.github/workflows/dependabot-review-signal.yml", import.meta.url),
  "utf8",
);
const reconciliation = await readFile(
  new URL("../.github/workflows/dependabot-auto-merge-reconcile.yml", import.meta.url),
  "utf8",
);
const contributing = await readFile(
  new URL("../CONTRIBUTING.adoc", import.meta.url),
  "utf8",
);
const makefile = await readFile(new URL("../Makefile.toml", import.meta.url), "utf8");
const reconciliationPullRequestFilter = new URL(
  "./dependabot-auto-merge-pr-numbers.jq",
  import.meta.url,
);
const reconciliationDisableResultFilter = new URL(
  "./dependabot-auto-merge-disable-result.jq",
  import.meta.url,
);
const reconciliationPullRequestDetailFilter = new URL(
  "./dependabot-auto-merge-pr-detail.jq",
  import.meta.url,
);
const decide = controller.match(/\n  decide:\n[\s\S]*?(?=\n  revoke:\n)/)?.[0] ?? "";
const revoke = controller.match(/\n  revoke:\n[\s\S]*?(?=\n  enable:\n)/)?.[0] ?? "";
const enable = controller.match(
  /\n  enable:\n[\s\S]*?(?=\n  revoke-after-enable-failure:\n)/,
)?.[0] ?? "";
const revokeAfterEnableFailure = controller.match(
  /\n  revoke-after-enable-failure:\n[\s\S]*$/,
)?.[0] ?? "";

test("eligibility is a read-only pull request job check over trusted base code", () => {
  assert.match(eligibility, /\n  pull_request:\n/);
  assert.doesNotMatch(eligibility, /pull_request_target:/);
  assert.match(eligibility, /name:\s*dependabot \/ eligibility/);
  assert.match(eligibility, /dependabot\[bot\]/);
  assert.match(eligibility, /head\.repo\.full_name == github\.repository/);
  assert.doesNotMatch(eligibility, /ref:\s*\$\{\{\s*github\.event\.pull_request\.head/);
  assert.match(eligibility, /ref:\s*\$\{\{\s*github\.event\.pull_request\.base\.sha\s*\}\}/);
  assert.match(eligibility, /persist-credentials:\s*false/);
  assert.match(eligibility, /dependabot\/fetch-metadata@[0-9a-f]{40}/);
  assert.doesNotMatch(eligibility, /alert-lookup:/);
  assert.match(eligibility, /vulnerability-alerts:\s*read/);
  assert.match(eligibility, /headSha:\s*\$head_sha/);
  assert.match(eligibility, /dependabot-alert-snapshot\.sh/);
  assert.match(eligibility, /securityUpdate:\s*\$security_alerts\[0\]\.securityUpdate/);
  assert.match(eligibility, /case "\$MAINTAINER_CHANGES" in/);
  assert.match(eligibility, /true\|false\) maintainer_changes=/);
  assert.match(eligibility, /jq -e '\.eligible == true'/);
  assert.doesNotMatch(eligibility, /check-runs|checks:\s*write|--method POST/);
  assert.doesNotMatch(eligibility, /issues:\s*write/);
});

test("security alert inventory paginates beyond 100 and fails closed", () => {
  const helperCall = /tools\/dependabot-alert-snapshot\.sh/g;
  assert.equal([...eligibility.matchAll(helperCall)].length, 1);
  assert.equal([...decide.matchAll(helperCall)].length, 1);
  assert.equal([...enable.matchAll(helperCall)].length, 1);
  assert.match(eligibility, /security-alert-snapshot\.json/);
  assert.match(decide, /security-alert-snapshot\.json/);
  assert.match(enable, /enable-security-alert-snapshot\.json/);
  assert.doesNotMatch(eligibility, /secrets\.|PAT|personal.access/i);
});

test("review changes emit a read-only signal and reuse successful current-base CI", () => {
  assert.match(reviewSignal, /\n  pull_request_review:\n/);
  assert.match(reviewSignal, /types:\s*\[submitted, dismissed\]/);
  assert.match(reviewSignal, /^permissions:\s*\{\}/m);
  assert.match(reviewSignal, /permissions:\s*\{\}/g);
  assert.match(reviewSignal, /dependabot\[bot\]/);
  assert.match(reviewSignal, /head\.repo\.full_name == github\.repository/);
  assert.doesNotMatch(reviewSignal, /checkout|gh api|pull-requests:\s*write|contents:\s*write/);
  assert.match(controller, /workflows:\s*\["CI and Release", "Dependabot review signal"\]/);
  assert.match(controller, /workflow_run\.name == 'Dependabot review signal'/);
  assert.equal(
    [...controller.matchAll(
      /actions\/runs\?event=pull_request&head_sha=.*&status=success&per_page=100/g,
    )].length,
    2,
  );
  assert.equal(
    [...controller.matchAll(/no successful current-base CI run/g)].length,
    2,
  );
  assert.match(controller, /\.actor\.login == "dependabot\[bot\]"/);
});

test("controller runs only after CI and keeps mutation in a narrow trusted job", () => {
  assert.match(controller, /workflow_run:/);
  assert.match(
    controller,
    /workflows:\s*\["CI and Release", "Dependabot review signal"\]/,
  );
  assert.match(controller, /types:\s*\[completed\]/);
  assert.match(controller, /pull-requests:\s*write/);
  assert.doesNotMatch(controller, /issues:\s*write/);
  assert.doesNotMatch(controller, /pull_request_target:/);
  assert.doesNotMatch(controller, /gh pr merge/);
  assert.match(
    controller,
    /group:\s*>-[\s\S]*dependabot-auto-merge-\$\{\{[\s\S]*pull_requests\[0\]\.number/,
  );
  assert.match(controller, /cancel-in-progress:\s*false/);
  assert.match(controller, /enablePullRequestAutoMerge/);
  assert.match(controller, /\$clientMutationId:\s*String!/);
  assert.match(controller, /expectedHeadOid:\s*\$head/);
  assert.match(controller, /clientMutationId:\s*\$clientMutationId/);
  assert.match(controller, /-f head="\$EXPECTED_HEAD_OID"/);
  assert.match(controller, /EXPECTED_HEAD_OID:\s*\$\{\{\s*needs\.decide\.outputs\.expected_head_oid\s*\}\}/);
  assert.match(controller, /mergeMethod:\s*SQUASH/);
  assert.match(controller, /--slurpfile ci_run ci-run\.json/);
  assert.match(controller, /headSha:\s*\$ci\.head_sha/);
  assert.match(controller, /baseSha:\s*\$ci\.verified_base_sha/);
  assert.match(controller, /github\.event\.workflow_run\.pull_requests\[0\]\.base\.sha/);
  assert.match(controller, /dependabot \/ eligibility/);
  assert.match(controller, /appSlug:\s*\.app\.slug, appId:\s*\.app\.id/);
  assert.equal(
    [...controller.matchAll(
      /select\(\.state == "APPROVED" and \.user\.type == "User"\)/g,
    )].length,
    2,
  );
  assert.equal([...controller.matchAll(/sort_by\(\.id\)/g)].length, 2);
  assert.doesNotMatch(controller, /sort_by\(\.completed_at\)/);
  assert.match(controller, /--slurpfile changed_files changed-files\.json/);
  assert.match(controller, /changedFiles:\s*\$changed_files\[0\]/);
  assert.match(controller, /ref:\s*\$\{\{\s*github\.event\.workflow_run\.pull_requests\[0\]\.base\.sha\s*\}\}/);
  assert.match(controller, /expected_base_sha:\s*\$\{\{\s*steps\.context\.outputs\.base_sha\s*\}\}/);
  assert.match(controller, /ref:\s*\$\{\{\s*needs\.decide\.outputs\.expected_base_sha\s*\}\}/);
  assert.match(controller, /test "\$\(jq -r \.head\.sha enable-pr\.json\)" = "\$EXPECTED_HEAD_OID"/);
  assert.match(controller, /test "\$\(jq -r \.base\.sha enable-pr\.json\)" = "\$EXPECTED_BASE_SHA"/);
  assert.match(controller, /test "\$\(cat enable-base-sha\.txt\)" = "\$EXPECTED_BASE_SHA"/);
  assert.match(controller, /gh api "repos\/\$GITHUB_REPOSITORY\/branches\/main"/);
  assert.match(enable, /--jq \.default_branch/);
  assert.match(enable, /test "\$\(cat enable-default-branch\.txt\)" = "main"/);
  assert.match(controller, /vulnerability-alerts:\s*read/g);
  assert.match(controller, /rulesets\?includes_parents=true&per_page=100/);
  assert.match(controller, /strict-rulesets \.github\/dependabot-auto-merge-policy\.json/);
  assert.match(controller, /jq -e '\.eligible == true' strict-ruleset-decision\.json/);
  assert.equal(
    [...controller.matchAll(
      /controller \.github\/dependabot-auto-merge-policy\.json/g,
    )].length,
    2,
  );
  assert.match(enable, /checks:\s*read/);
  assert.match(enable, /pulls\/\$PR_NUMBER\/files\?per_page=100/);
  assert.match(enable, /commits\/\$EXPECTED_HEAD_OID\/check-runs\?per_page=100/);
  assert.match(enable, /pulls\/\$PR_NUMBER\/reviews\?per_page=100/);
  assert.match(enable, /enable-controller-input\.json/);
  assert.match(enable, /jq -e '\.eligible == true' enable-controller-decision\.json/);
  assert.match(enable, /enable-auto-merge-result\.json/);
  assert.match(enable, /enable-audit-record\.json/);
  assert.match(enable, /clientMutationId:\s*\$clientMutationId/);
  assert.match(enable, /alertObservation:\s*\$alerts\[0\]/);
  assert.match(enable, /strictRulesetDecision:\s*\$ruleset\[0\]/);
  assert.match(
    enable,
    /\.data\.enablePullRequestAutoMerge\.pullRequest\.number == \$pr_number/,
  );
  assert.match(enable, /actions:\s*read/);
  assert.ok(
    enable.indexOf("enable-controller-decision.json")
      < enable.indexOf("enablePullRequestAutoMerge"),
  );
  assert.ok(
    enable.indexOf("strict-ruleset-decision.json")
      < enable.indexOf("enable-security-alert-snapshot.json"),
  );
  for (const priorObservation of [
    "enable-checks.json",
    "enable-reviews.json",
    "enable-ci-run.json",
  ]) {
    assert.ok(
      enable.indexOf(priorObservation)
        < enable.indexOf("enable-security-alert-snapshot.json"),
      priorObservation,
    );
  }
  assert.ok(
    enable.indexOf("enable-security-alert-snapshot.json")
      < enable.indexOf("enable-controller-decision.json"),
  );
});

test("a failed final verification revokes a previously enabled request", () => {
  assert.match(revokeAfterEnableFailure, /needs:\s*\[decide, enable\]/);
  assert.match(revokeAfterEnableFailure, /always\(\)/);
  assert.match(revokeAfterEnableFailure, /needs\.decide\.outputs\.eligible == 'true'/);
  assert.match(revokeAfterEnableFailure, /needs\.enable\.result != 'success'/);
  assert.match(revokeAfterEnableFailure, /pull-requests:\s*write/);
  assert.match(
    revokeAfterEnableFailure,
    /\.node_id == \$expected_node_id/,
  );
  assert.match(revokeAfterEnableFailure, /\.user\.login == "dependabot\[bot\]"/);
  assert.match(revokeAfterEnableFailure, /disablePullRequestAutoMerge/);
  assert.match(
    revokeAfterEnableFailure,
    /\.data\.disablePullRequestAutoMerge\.pullRequest\.number == \$pr_number/,
  );
  assert.match(
    revokeAfterEnableFailure,
    /\.data\.disablePullRequestAutoMerge\.pullRequest\.autoMergeRequest == null/,
  );
  assert.doesNotMatch(
    revokeAfterEnableFailure,
    /checkout|contents:\s*write|checks:\s*write|enablePullRequestAutoMerge/,
  );
});

test("controller revokes auto-merge after failed or ineligible reevaluation", () => {
  assert.match(revoke, /always\(\)/);
  assert.match(revoke, /needs\.decide\.outputs\.eligible != 'true'/);
  assert.match(revoke, /pull-requests:\s*write/);
  assert.doesNotMatch(revoke, /contents:\s*write|actions:\s*write|checks:\s*write/);
  assert.match(revoke, /\.user\.login == "dependabot\[bot\]"/);
  assert.match(revoke, /\.head\.repo\.full_name == \$repository/);
  assert.match(revoke, /disablePullRequestAutoMerge/);
  assert.doesNotMatch(revoke, /enablePullRequestAutoMerge|checkout/);
});

test("repository changes continuously reconcile existing auto-merge requests", () => {
  assert.match(reconciliation, /\n  push:\n/);
  assert.match(reconciliation, /branches:\s*\[main\]/);
  assert.match(reconciliation, /dependabot-auto-merge-policy\.json/);
  assert.match(reconciliation, /\n  schedule:\n/);
  assert.match(reconciliation, /cron:\s*"\*\/5 \* \* \* \*"/);
  assert.match(reconciliation, /\n  workflow_dispatch:\n/);
  assert.match(reconciliation, /^permissions:\s*\{\}/m);
  assert.match(reconciliation, /pull-requests:\s*write/);
  assert.match(reconciliation, /actions:\s*read/);
  assert.match(reconciliation, /checks:\s*read/);
  assert.match(reconciliation, /vulnerability-alerts:\s*read/);
  assert.match(reconciliation, /dependabot-alert-snapshot\.sh/);
  assert.match(reconciliation, /dependabot-auto-merge-disable-result\.jq/);
  assert.match(reconciliation, /dependabot-auto-merge-pr-detail\.jq/);
  assert.match(reconciliation, /dependabot-auto-merge-pr-numbers\.jq/);
  assert.match(reconciliation, /rulesets\?includes_parents=true&per_page=100/);
  assert.match(
    reconciliation,
    /strict-rulesets \.github\/dependabot-auto-merge-policy\.json/,
  );
  assert.match(reconciliation, /reconciliation-strict-ruleset-decision\.json/);
  assert.match(reconciliation, /pulls\/\$pr_number\/files\?per_page=100/);
  assert.match(reconciliation, /commits\/\$head_sha\/check-runs\?per_page=100/);
  assert.match(reconciliation, /pulls\/\$pr_number\/reviews\?per_page=100/);
  assert.match(
    reconciliation,
    /actions\/runs\?event=pull_request&head_sha=\$head_sha&status=success&per_page=100/,
  );
  assert.match(reconciliation, /reconciliation-controller-input-\$pr_number\.json/);
  assert.match(reconciliation, /sort_by\(\.id\)/);
  assert.doesNotMatch(reconciliation, /sort_by\(\.completed_at\)/);
  assert.match(
    reconciliation,
    /controller \.github\/dependabot-auto-merge-policy\.json/,
  );
  assert.match(reconciliation, /reconciliation-audit-\$pr_number\.json/);
  assert.equal(
    [...reconciliation.matchAll(
      /reconcile \.github\/dependabot-auto-merge-policy\.json/g,
    )].length,
    2,
  );
  assert.match(
    reconciliation,
    /reconciliation-policy-decision\.json[\s\S]*exit 0[\s\S]*tools\/dependabot-alert-snapshot\.sh/,
  );
  assert.match(reconciliation, /steps\.safety\.outputs\.safe != 'true'/);
  assert.match(reconciliation, /steps\.safety\.outcome == 'failure'/);
  assert.match(reconciliation, /disablePullRequestAutoMerge/);
  assert.doesNotMatch(reconciliation, /enablePullRequestAutoMerge|pull_request_target:/);
});

function filterReconciliationPullRequests(pages) {
  return spawnSync(
    "jq",
    ["-e", "-s", "-f", reconciliationPullRequestFilter.pathname],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        GITHUB_REPOSITORY: "KeishiS/adocweave",
      },
      input: pages.map((page) => JSON.stringify(page)).join("\n"),
    },
  );
}

function pullRequest(number, overrides = {}) {
  return {
    number,
    user: { login: "dependabot[bot]" },
    base: {
      ref: "main",
      repo: { full_name: "KeishiS/adocweave" },
    },
    head: {
      ref: `dependabot/cargo/dependency-${number}`,
      repo: { full_name: "KeishiS/adocweave" },
    },
    ...overrides,
  };
}

function validateReconciliationDisableResult(result, pullRequestNumber = 17) {
  return spawnSync(
    "jq",
    [
      "-e",
      "--argjson",
      "pr_number",
      String(pullRequestNumber),
      "-f",
      reconciliationDisableResultFilter.pathname,
    ],
    {
      encoding: "utf8",
      input: JSON.stringify(result),
    },
  );
}

function validateReconciliationPullRequestDetail(
  pullRequestDetail,
  pullRequestNumber = 17,
) {
  return spawnSync(
    "jq",
    [
      "-e",
      "--argjson",
      "pr_number",
      String(pullRequestNumber),
      "--arg",
      "repository",
      "KeishiS/adocweave",
      "-f",
      reconciliationPullRequestDetailFilter.pathname,
    ],
    {
      encoding: "utf8",
      input: JSON.stringify(pullRequestDetail),
    },
  );
}

function pullRequestDetail(autoMerge = null) {
  return {
    ...pullRequest(17),
    node_id: "PR_kwDOExample17",
    auto_merge: autoMerge,
  };
}

test("reconciliation accepts an empty Dependabot Pull Request set", () => {
  const result = filterReconciliationPullRequests([[]]);

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), []);
});

test("reconciliation selects Dependabot Pull Requests across every API page", () => {
  const result = filterReconciliationPullRequests([
    [
      pullRequest(17),
      pullRequest(18, { user: { login: "contributor" } }),
    ],
    [
      pullRequest(19),
      pullRequest(20, {
        head: {
          ref: "feature/not-dependabot",
          repo: { full_name: "KeishiS/adocweave" },
        },
      }),
    ],
  ]);

  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), [17, 19]);
});

test("reconciliation fails closed for an invalid Pull Request API response", () => {
  const missingResponse = filterReconciliationPullRequests([]);
  const invalidPage = filterReconciliationPullRequests([
    { message: "Resource not accessible by integration" },
  ]);
  const invalidEntry = filterReconciliationPullRequests([
    [{ number: 17, user: null }],
  ]);

  assert.notEqual(missingResponse.status, 0);
  assert.match(missingResponse.stderr, /must contain one or more arrays/);
  assert.notEqual(invalidPage.status, 0);
  assert.match(invalidPage.stderr, /must contain one or more arrays/);
  assert.notEqual(invalidEntry.status, 0);
  assert.match(invalidEntry.stderr, /contains an invalid entry/);
  for (const number of [0, -1, 1.5]) {
    const invalidNumber = filterReconciliationPullRequests([
      [pullRequest(number)],
    ]);
    assert.notEqual(invalidNumber.status, 0, String(number));
    assert.match(invalidNumber.stderr, /contains an invalid entry/);
  }
});

test("reconciliation requires a confirmed GraphQL auto-merge cancellation", () => {
  const success = {
    data: {
      disablePullRequestAutoMerge: {
        pullRequest: {
          number: 17,
          autoMergeRequest: null,
        },
      },
    },
  };
  const graphQlError = {
    ...success,
    errors: [{ message: "auto-merge could not be disabled" }],
  };
  const stillEnabled = structuredClone(success);
  stillEnabled.data.disablePullRequestAutoMerge.pullRequest.autoMergeRequest = {
    enabledAt: "2026-07-29T21:00:00Z",
  };
  const missingAutoMergeRequest = structuredClone(success);
  delete missingAutoMergeRequest.data.disablePullRequestAutoMerge.pullRequest
    .autoMergeRequest;
  const missingNumber = structuredClone(success);
  delete missingNumber.data.disablePullRequestAutoMerge.pullRequest.number;
  const incompleteResponses = [
    null,
    {},
    { data: null },
    { data: {} },
    { data: { disablePullRequestAutoMerge: null } },
    { data: { disablePullRequestAutoMerge: {} } },
    { data: { disablePullRequestAutoMerge: { pullRequest: null } } },
    missingNumber,
    missingAutoMergeRequest,
  ];

  assert.equal(validateReconciliationDisableResult(success).status, 0);
  assert.notEqual(validateReconciliationDisableResult(graphQlError).status, 0);
  assert.notEqual(validateReconciliationDisableResult(stillEnabled).status, 0);
  assert.notEqual(validateReconciliationDisableResult(success, 18).status, 0);
  for (const incompleteResponse of incompleteResponses) {
    assert.notEqual(
      validateReconciliationDisableResult(incompleteResponse).status,
      0,
      JSON.stringify(incompleteResponse),
    );
  }
});

test("reconciliation trusts only a validated Pull Request detail response", () => {
  const disabled = validateReconciliationPullRequestDetail(pullRequestDetail());
  const enabled = validateReconciliationPullRequestDetail(
    pullRequestDetail({
      enabled_by: { login: "maintainer" },
      merge_method: "squash",
      commit_title: null,
      commit_message: null,
    }),
  );

  assert.equal(disabled.status, 0, disabled.stderr);
  assert.deepEqual(JSON.parse(disabled.stdout), {
    nodeId: "PR_kwDOExample17",
    autoMergeEnabled: false,
    autoMergeStateKnown: true,
    autoMergeMethod: null,
  });
  assert.equal(enabled.status, 0, enabled.stderr);
  assert.deepEqual(JSON.parse(enabled.stdout), {
    nodeId: "PR_kwDOExample17",
    autoMergeEnabled: true,
    autoMergeStateKnown: true,
    autoMergeMethod: "squash",
  });
});

test("reconciliation rejects missing or drifted Pull Request identity", () => {
  const invalidResponses = [
    {},
    { ...pullRequestDetail(), number: 18 },
    { ...pullRequestDetail(), node_id: "" },
    { ...pullRequestDetail(), user: { login: "contributor" } },
    {
      ...pullRequestDetail(),
      base: {
        ref: "release",
        repo: { full_name: "KeishiS/adocweave" },
      },
    },
    {
      ...pullRequestDetail(),
      head: {
        ref: "dependabot/cargo/dependency-17",
        repo: { full_name: "fork/adocweave" },
      },
    },
    {
      ...pullRequestDetail(),
      head: {
        ref: "feature/not-dependabot",
        repo: { full_name: "KeishiS/adocweave" },
      },
    },
  ];

  for (const response of invalidResponses) {
    const result = validateReconciliationPullRequestDetail(response);
    assert.notEqual(result.status, 0, JSON.stringify(response));
    assert.match(result.stderr, /detail response identity changed/);
  }
});

test("reconciliation marks an unknown auto-merge detail for cancellation", () => {
  const missingAutoMerge = pullRequestDetail();
  delete missingAutoMerge.auto_merge;
  for (const response of [
    missingAutoMerge,
    ...[
    "enabled",
    {},
    {
      enabled_by: { login: "" },
      merge_method: "squash",
      commit_title: null,
      commit_message: null,
    },
    {
      enabled_by: { login: "maintainer" },
      merge_method: "unsupported",
      commit_title: null,
      commit_message: null,
    },
    ].map((autoMerge) => pullRequestDetail(autoMerge)),
  ]) {
    const result = validateReconciliationPullRequestDetail(
      response,
    );
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(JSON.parse(result.stdout), {
      nodeId: "PR_kwDOExample17",
      autoMergeEnabled: Object.hasOwn(response, "auto_merge")
        && response.auto_merge != null,
      autoMergeStateKnown: false,
      autoMergeMethod: null,
    });
  }
  assert.match(reconciliation, /autoMergeStateKnown != true/);
});

test("reconciliation can identify non-squash requests before disabling them", () => {
  for (const mergeMethod of ["merge", "rebase"]) {
    const result = validateReconciliationPullRequestDetail(
      pullRequestDetail({
        enabled_by: { login: "maintainer" },
        merge_method: mergeMethod,
        commit_title: null,
        commit_message: null,
      }),
    );
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(JSON.parse(result.stdout), {
      nodeId: "PR_kwDOExample17",
      autoMergeEnabled: true,
      autoMergeStateKnown: true,
      autoMergeMethod: mergeMethod,
    });
  }
  assert.match(reconciliation, /autoMergeMethod == "squash"/);
});

test("automation changes require a frozen two-stage procedure", () => {
  assert.match(
    contributing,
    /最初のPull Requestでは\s*policyの``enabled``だけを``false``へ変更/,
  );
  assert.match(
    contributing,
    /既存の\s*Dependabot Pull Requestからauto-mergeが解除されたことをGitHub APIで確認/,
  );
  assert.match(
    contributing,
    /その確認後に、別の\s*Pull Requestでworkflowまたは判定toolを変更/,
  );
  assert.match(
    contributing,
    /定期監査はこの時間差を解消する保証ではありません/,
  );
  assert.match(
    contributing,
    /同じPull Requestに対する判定は同時実行せず、実行中の判定を取り消さずに直列化/,
  );
  assert.match(contributing, /OPEN alertを最後に取得/);
  assert.match(
    contributing,
    /head、base、check、review、変更fileおよび現在のbaseに対するCI/,
  );
  assert.match(
    contributing,
    /auto-merge方式の採否を決定するまで、policyの``enabled``は``false``のまま維持/,
  );
});

test("workflow permissions remain scoped to the jobs that need them", () => {
  assert.match(eligibility, /^permissions:\s*\{\}/m);
  assert.match(controller, /^permissions:\s*\{\}/m);
  assert.doesNotMatch(eligibility, /issues:\s*write|contents:\s*write|pull-requests:\s*write/);
  assert.doesNotMatch(controller, /issues:\s*write|checks:\s*write/);
  assert.doesNotMatch(eligibility, /security-events:/);
  assert.match(makefile, /unknown permission scope "vulnerability-alerts"/);
});

test("all actions are pinned and pull request code is never checked out", () => {
  for (const workflow of [eligibility, controller, reviewSignal, reconciliation]) {
    for (const reference of workflow.matchAll(/uses:\s*([^\s#]+)/g)) {
      assert.match(reference[1], /@[0-9a-f]{40}$/, reference[1]);
    }
    assert.doesNotMatch(workflow, /persist-credentials:\s*true/);
    assert.doesNotMatch(workflow, /checkout[^\n]*\n(?:.*\n){0,8}.*head\.sha/);
  }
});

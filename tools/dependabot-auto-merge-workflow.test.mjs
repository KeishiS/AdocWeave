import assert from "node:assert/strict";
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
const makefile = await readFile(new URL("../Makefile.toml", import.meta.url), "utf8");
const decide = controller.match(/\n  decide:\n[\s\S]*?(?=\n  enable:\n)/)?.[0] ?? "";
const enable = controller.match(/\n  enable:\n[\s\S]*$/)?.[0] ?? "";

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
  assert.match(eligibility, /alert-lookup:\s*true/);
  assert.match(eligibility, /vulnerability-alerts:\s*read/);
  assert.match(eligibility, /headSha:\s*\$head_sha/);
  assert.match(eligibility, /SECURITY_ADVISORY_ID:.*outputs\.ghsa-id/);
  assert.match(eligibility, /SECURITY_ALERT_STATE:.*outputs\.alert-state/);
  assert.match(eligibility, /securityUpdate:\s*\$security_update/);
  assert.match(eligibility, /jq -e '\.eligible == true'/);
  assert.doesNotMatch(eligibility, /check-runs|checks:\s*write|--method POST/);
  assert.doesNotMatch(eligibility, /issues:\s*write/);
});

test("security alert inventory paginates beyond 100 and fails closed", () => {
  const helperCall = /tools\/dependabot-alert-inventory\.sh "\$GITHUB_REPOSITORY"/g;
  assert.equal([...eligibility.matchAll(helperCall)].length, 1);
  assert.equal([...decide.matchAll(helperCall)].length, 1);
  assert.equal([...enable.matchAll(helperCall)].length, 1);
  assert.match(eligibility, /lookup_completed=true/);
  assert.match(eligibility, /open_count=\$open_count/);
  assert.match(eligibility, /--argjson open_security_alerts "\$OPEN_SECURITY_ALERTS"/);
  assert.doesNotMatch(eligibility, /secrets\.|PAT|personal.access/i);
});

test("controller runs only after CI and keeps mutation in a narrow trusted job", () => {
  assert.match(controller, /workflow_run:/);
  assert.match(controller, /workflows:\s*\["CI and Release"\]/);
  assert.match(controller, /types:\s*\[completed\]/);
  assert.match(controller, /pull-requests:\s*write/);
  assert.doesNotMatch(controller, /issues:\s*write/);
  assert.doesNotMatch(controller, /pull_request_target:/);
  assert.doesNotMatch(controller, /gh pr merge/);
  assert.match(controller, /enablePullRequestAutoMerge/);
  assert.match(controller, /mutation\(\$pullRequest: ID!, \$head: GitObjectID!\)/);
  assert.match(controller, /expectedHeadOid:\s*\$head/);
  assert.match(controller, /-f head="\$EXPECTED_HEAD_OID"/);
  assert.match(controller, /EXPECTED_HEAD_OID:\s*\$\{\{\s*needs\.decide\.outputs\.expected_head_oid\s*\}\}/);
  assert.match(controller, /mergeMethod:\s*SQUASH/);
  assert.match(controller, /github\.event\.workflow_run\.head_sha/);
  assert.match(controller, /github\.event\.workflow_run\.pull_requests\[0\]\.base\.sha/);
  assert.match(controller, /dependabot \/ eligibility/);
  assert.match(controller, /appSlug:\s*\.app\.slug, appId:\s*\.app\.id/);
  assert.equal(
    [...controller.matchAll(
      /select\(\.state == "APPROVED" and \.user\.type == "User"\)/g,
    )].length,
    2,
  );
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
  assert.ok(
    enable.indexOf("enable-controller-decision.json")
      < enable.indexOf("enablePullRequestAutoMerge"),
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
  for (const workflow of [eligibility, controller]) {
    for (const reference of workflow.matchAll(/uses:\s*([^\s#]+)/g)) {
      assert.match(reference[1], /@[0-9a-f]{40}$/, reference[1]);
    }
    assert.doesNotMatch(workflow, /persist-credentials:\s*true/);
    assert.doesNotMatch(workflow, /checkout[^\n]*\n(?:.*\n){0,8}.*head\.sha/);
  }
});

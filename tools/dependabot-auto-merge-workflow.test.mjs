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

test("eligibility uses the trusted base workflow and binds its result to the head SHA", () => {
  assert.match(eligibility, /pull_request_target:/);
  assert.match(eligibility, /dependabot\[bot\]/);
  assert.match(eligibility, /head\.repo\.full_name == github\.repository/);
  assert.doesNotMatch(eligibility, /ref:\s*\$\{\{\s*github\.event\.pull_request\.head/);
  assert.match(eligibility, /ref:\s*\$\{\{\s*github\.event\.pull_request\.base\.sha\s*\}\}/);
  assert.match(eligibility, /persist-credentials:\s*false/);
  assert.match(eligibility, /dependabot\/fetch-metadata@[0-9a-f]{40}/);
  assert.match(eligibility, /alert-lookup:\s*true/);
  assert.match(eligibility, /security-events:\s*read/);
  assert.match(eligibility, /steps\.metadata\.outputs\.alert-state/);
  assert.match(eligibility, /steps\.metadata\.outputs\.ghsa-id/);
  assert.match(eligibility, /"head_sha":\s*\$head_sha/);
  assert.match(eligibility, /"name":\s*"dependabot \/ eligibility"/);
  assert.match(eligibility, /conclusion/);
  assert.doesNotMatch(eligibility, /issues:\s*write/);
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
  assert.match(controller, /ref:\s*\$\{\{\s*github\.event\.workflow_run\.pull_requests\[0\]\.base\.sha\s*\}\}/);
});

test("workflow permissions remain scoped to the jobs that need them", () => {
  assert.match(eligibility, /^permissions:\s*\{\}/m);
  assert.match(controller, /^permissions:\s*\{\}/m);
  assert.doesNotMatch(eligibility, /issues:\s*write|contents:\s*write|pull-requests:\s*write/);
  assert.doesNotMatch(controller, /issues:\s*write|checks:\s*write/);
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

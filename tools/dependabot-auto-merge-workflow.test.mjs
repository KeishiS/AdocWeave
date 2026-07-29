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
  assert.match(eligibility, /ref:\s*\$\{\{\s*github\.event\.repository\.default_branch\s*\}\}/);
  assert.match(eligibility, /persist-credentials:\s*false/);
  assert.match(eligibility, /dependabot\/fetch-metadata@[0-9a-f]{40}/);
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
  assert.match(controller, /expectedHeadOid:\s*\$head/);
  assert.match(controller, /mergeMethod:\s*SQUASH/);
  assert.match(controller, /github\.event\.workflow_run\.head_sha/);
  assert.match(controller, /github\.event\.workflow_run\.pull_requests\[0\]\.base\.sha/);
  assert.match(controller, /dependabot \/ eligibility/);
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

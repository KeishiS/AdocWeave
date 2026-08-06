import assert from "node:assert/strict";
import test from "node:test";

import { validateReadinessEvidence } from "./release-readiness.mjs";

const SHA = "1234567890abcdef1234567890abcdef12345678";

function evidence() {
  return {
    candidateSha: SHA,
    finalizationPullRequest: "42",
    defaultBranch: "main",
    defaultBranchSha: SHA,
    intent: { schemaVersion: 1, version: "1.2.3", state: "ready", generation: 8 },
    previousIntent: { schemaVersion: 1, version: "1.2.3", state: "preparing", generation: 8 },
    packageVersion: "1.2.3",
    candidateCommit: {
      sha: SHA,
      parents: [{ sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }],
    },
    pullRequest: {
      number: 42,
      state: "closed",
      merged_at: "2026-08-05T00:00:00Z",
      merge_commit_sha: SHA,
      base: { ref: "main", sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" },
    },
    pullRequestFiles: [{ filename: "release/intent.json" }],
    openPullRequests: [],
    successfulCandidateRuns: [{
      id: 1234,
      head_branch: "main",
      head_sha: SHA,
      event: "push",
      status: "completed",
      conclusion: "success",
    }],
    tagExists: false,
    releaseExists: false,
  };
}

test("review済みfinal candidateをexact SHAとrun IDへ固定する", () => {
  assert.deepEqual(validateReadinessEvidence(evidence()), {
    candidateSha: SHA,
    pullRequestNumber: 42,
    runId: 1234,
    tag: "v1.2.3",
  });
});

test("finalization Pull Requestとintent遷移の不一致を拒否する", () => {
  for (const mutate of [
    (value) => { value.pullRequest.state = "open"; },
    (value) => { value.pullRequest.merged_at = null; },
    (value) => { value.pullRequest.base.ref = "release"; },
    (value) => { value.pullRequest.merge_commit_sha = "a".repeat(40); },
    (value) => { value.candidateCommit.sha = "a".repeat(40); },
    (value) => { value.candidateCommit.parents = []; },
    (value) => { value.candidateCommit.parents.push({ sha: "b".repeat(40) }); },
    (value) => { value.previousIntent.state = "ready"; },
    (value) => { value.previousIntent.generation = 7; },
    (value) => { value.intent.state = "preparing"; },
    (value) => { value.pullRequestFiles.push({ filename: "README.adoc" }); },
  ]) {
    const value = evidence();
    mutate(value);
    assert.throws(() => validateReadinessEvidence(value));
  }
});

test("未固定のrepository状態と既存公開対象をfail-closedに拒否する", () => {
  for (const [mutate, pattern] of [
    [(value) => { value.defaultBranchSha = "a".repeat(40); }, /main先端/],
    [(value) => { value.openPullRequests = [{ number: 99 }]; }, /open Pull Request/],
    [(value) => { value.successfulCandidateRuns = []; }, /release candidate/],
    [(value) => { value.successfulCandidateRuns[0].conclusion = "failure"; }, /release candidate/],
    [(value) => { value.tagExists = true; }, /tag v1.2.3/],
    [(value) => { value.releaseExists = true; }, /Release v1.2.3/],
  ]) {
    const value = evidence();
    mutate(value);
    assert.throws(() => validateReadinessEvidence(value), pattern);
  }
});

test("不正なSHAとPull Request番号を拒否する", () => {
  for (const mutate of [
    (value) => { value.candidateSha = "ABC"; },
    (value) => { value.finalizationPullRequest = "0"; },
    (value) => { value.finalizationPullRequest = "01"; },
  ]) {
    const value = evidence();
    mutate(value);
    assert.throws(() => validateReadinessEvidence(value));
  }
});

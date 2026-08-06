import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  assertTagAbsent,
  collectReadinessEvidence,
  readCandidateChangedPaths,
  validateReadinessEvidence,
} from "./release-readiness.mjs";

const SHA = "1234567890abcdef1234567890abcdef12345678";
const PARENT_SHA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PACKAGE_VERSION = JSON.parse(
  readFileSync(new URL("../release-manifest.json", import.meta.url), "utf8"),
).packageVersion;

function jsonResponse(value, status = 200) {
  return {
    status,
    ok: status >= 200 && status < 300,
    async json() {
      return structuredClone(value);
    },
  };
}

function apiFixture({ failPath, unboundedOpenPullRequests = false } = {}) {
  const calls = [];
  const fetchImpl = async (input) => {
    const url = new URL(String(input));
    const path = url.pathname.replace("/repos/example/adocweave/", "");
    calls.push(url);
    if (path === failPath) return jsonResponse({ message: "failure" }, 503);
    if (path === "") return jsonResponse({ default_branch: "main" });
    if (path === "commits/main") return jsonResponse({ sha: SHA });
    if (path === `commits/${SHA}`) {
      return jsonResponse({ sha: SHA, parents: [{ sha: PARENT_SHA }] });
    }
    if (path === "pulls/42") {
      return jsonResponse({
        number: 42,
        state: "closed",
        merged_at: "2026-08-05T00:00:00Z",
        merge_commit_sha: SHA,
        base: { ref: "main", sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
      });
    }
    if (path === "pulls/42/files") return jsonResponse([{ filename: "release/intent.json" }]);
    if (path === "pulls") {
      if (unboundedOpenPullRequests) {
        return jsonResponse(Array.from({ length: 100 }, (_, index) => ({ number: index + 1 })));
      }
      const page = Number(url.searchParams.get("page"));
      return jsonResponse(page === 1
        ? Array.from({ length: 100 }, (_, index) => ({ number: index + 1 }))
        : [{ number: 101 }]);
    }
    if (path === "actions/workflows/release.yml/runs") {
      const page = Number(url.searchParams.get("page"));
      const runs = page === 1
        ? Array.from({ length: 100 }, (_, index) => ({ id: index + 1 }))
        : [{
            id: 1234,
            head_branch: "main",
            head_sha: SHA,
            event: "push",
            status: "completed",
            conclusion: "success",
          }];
      return jsonResponse({ workflow_runs: runs });
    }
    if (path === `git/ref/tags/v${PACKAGE_VERSION}` ||
        path === `releases/tags/v${PACKAGE_VERSION}`) {
      return jsonResponse({ message: "Not Found" }, 404);
    }
    if (path === "contents/release/intent.json") {
      assert.equal(url.searchParams.get("ref"), PARENT_SHA);
      return jsonResponse({
        encoding: "base64",
        content: Buffer.from(JSON.stringify({
          schemaVersion: 1,
          version: PACKAGE_VERSION,
          state: "preparing",
          generation: 1,
        })).toString("base64"),
      });
    }
    throw new Error(`想定外のGitHub API呼び出しです: ${url}`);
  };
  return { calls, fetchImpl };
}

function collect(fetchImpl, readChangedPaths = () => ["release/intent.json"]) {
  return collectReadinessEvidence({
    repository: "example/adocweave",
    token: "test-token",
    candidateSha: SHA,
    dispatchSha: SHA,
    finalizationPullRequest: "42",
    fetchImpl,
    readChangedPaths,
  });
}

function evidence() {
  return {
    candidateSha: SHA,
    dispatchSha: SHA,
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
    candidateChangedPaths: ["release/intent.json"],
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
    (value) => { value.candidateChangedPaths = []; },
    (value) => { value.candidateChangedPaths.push("README.adoc"); },
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
    (value) => { value.dispatchSha = "ABC"; },
    (value) => { value.dispatchSha = "b".repeat(40); },
    (value) => { value.finalizationPullRequest = "0"; },
    (value) => { value.finalizationPullRequest = "01"; },
  ]) {
    const value = evidence();
    mutate(value);
    assert.throws(() => validateReadinessEvidence(value));
  }
});

test("GitHub APIから複数pageを取得しcandidateの親から直前intentを読む", async () => {
  const { calls, fetchImpl } = apiFixture();
  const result = await collect(fetchImpl);
  assert.equal(result.openPullRequests.length, 101);
  assert.equal(result.successfulCandidateRuns.length, 101);
  assert.deepEqual(result.previousIntent, {
    schemaVersion: 1,
    version: PACKAGE_VERSION,
    state: "preparing",
    generation: 1,
  });
  assert.equal(result.tagExists, false);
  assert.equal(result.releaseExists, false);
  assert.equal(calls.filter((url) => url.pathname.endsWith("/pulls")).length, 2);
  assert.equal(calls.filter((url) => url.pathname.endsWith("/runs")).length, 2);
});

test("candidateと単一parentのtree差分pathをNUL区切りで取得する", () => {
  const calls = [];
  const paths = readCandidateChangedPaths({
    candidateSha: SHA,
    parentSha: PARENT_SHA,
    root: new URL("../", import.meta.url),
    execFile(command, args, options) {
      calls.push({ command, args, options });
      return "release/intent.json\0docs/README.adoc\0";
    },
  });
  assert.deepEqual(paths, ["release/intent.json", "docs/README.adoc"]);
  assert.deepEqual(calls[0].args, [
    "diff-tree",
    "--no-commit-id",
    "--name-only",
    "-r",
    "-z",
    PARENT_SHA,
    SHA,
  ]);
});

test("Git tree差分の取得失敗をfail-closedにする", async () => {
  const { fetchImpl } = apiFixture();
  await assert.rejects(
    () => collect(fetchImpl, () => { throw new Error("git command failed"); }),
    /git command failed/,
  );
});

test("GitHub APIのHTTP失敗をfail-closedにする", async () => {
  const tagPath = `git/ref/tags/v${PACKAGE_VERSION}`;
  const { fetchImpl } = apiFixture({ failPath: tagPath });
  await assert.rejects(
    () => collect(fetchImpl),
    { message: `GitHub API ${tagPath} がHTTP 503を返しました` },
  );
});

test("公開直前のtag再確認は404だけを不存在として扱う", async () => {
  const path = "git/ref/tags/v1.2.3";
  await assertTagAbsent({
    repository: "example/adocweave",
    token: "test-token",
    tag: "v1.2.3",
    fetchImpl: async () => jsonResponse({ message: "Not Found" }, 404),
  });
  await assert.rejects(
    () => assertTagAbsent({
      repository: "example/adocweave",
      token: "test-token",
      tag: "v1.2.3",
      fetchImpl: async () => jsonResponse({ ref: "refs/tags/v1.2.3" }),
    }),
    /すでに存在/,
  );
  for (const status of [403, 429, 500, 503]) {
    await assert.rejects(
      () => assertTagAbsent({
        repository: "example/adocweave",
        token: "test-token",
        tag: "v1.2.3",
        fetchImpl: async () => jsonResponse({ message: "failure" }, status),
      }),
      { message: `GitHub API ${path} がHTTP ${status}を返しました` },
    );
  }
  await assert.rejects(
    () => assertTagAbsent({
      repository: "example/adocweave",
      token: "test-token",
      tag: "v1.2.3",
      fetchImpl: async () => { throw new Error("network unavailable"); },
    }),
    /network unavailable/,
  );
  await assert.rejects(
    () => assertTagAbsent({
      repository: "example/adocweave",
      token: "test-token",
      tag: "latest",
      fetchImpl: async () => { throw new Error("呼ばれません"); },
    }),
    /stable tagが不正/,
  );
});

test("GitHub APIが100件を返し続ける場合はpage上限で停止する", async () => {
  const { calls, fetchImpl } = apiFixture({ unboundedOpenPullRequests: true });
  await assert.rejects(() => collect(fetchImpl), /GitHub API pulls がpage上限 100/);
  assert.equal(calls.filter((url) => url.pathname.endsWith("/pulls")).length, 100);
});

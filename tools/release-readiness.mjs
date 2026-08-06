import { readFileSync } from "node:fs";
import process from "node:process";

import { validateReleaseIntent } from "./release-intent.mjs";

const ROOT = new URL("../", import.meta.url);
const COMMIT_SHA = /^[0-9a-f]{40}$/;
const MAX_API_PAGES = 100;

function fail(message) {
  throw new Error(message);
}

function positivePullRequestNumber(value) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1 || String(parsed) !== String(value)) {
    fail("finalization Pull Request番号が不正です");
  }
  return parsed;
}

export function validateReadinessEvidence({
  candidateSha,
  finalizationPullRequest,
  defaultBranch,
  defaultBranchSha,
  intent,
  previousIntent,
  packageVersion,
  candidateCommit,
  pullRequest,
  pullRequestFiles,
  openPullRequests,
  successfulCandidateRuns,
  tagExists,
  releaseExists,
}) {
  if (typeof candidateSha !== "string" || !COMMIT_SHA.test(candidateSha)) {
    fail("candidate SHAは小文字40文字のGit commitである必要があります");
  }
  const pullRequestNumber = positivePullRequestNumber(finalizationPullRequest);
  if (defaultBranch !== "main" || defaultBranchSha !== candidateSha) {
    fail("candidate SHAがdispatch開始時点のmain先端と一致しません");
  }
  if (candidateCommit?.sha !== candidateSha || candidateCommit.parents?.length !== 1 ||
      !COMMIT_SHA.test(candidateCommit.parents[0]?.sha ?? "")) {
    fail("candidate commitは確認済みの単一親merge commitである必要があります");
  }
  validateReleaseIntent(intent, packageVersion, { requireReady: true });
  validateReleaseIntent(previousIntent, packageVersion);
  if (previousIntent.state !== "preparing" ||
      previousIntent.schemaVersion !== intent.schemaVersion ||
      previousIntent.version !== intent.version ||
      previousIntent.generation !== intent.generation) {
    fail("finalization Pull Requestは同じgenerationのintentだけをpreparingからreadyへ変更する必要があります");
  }
  if (pullRequest?.number !== pullRequestNumber || pullRequest.state !== "closed" ||
      pullRequest.merged_at == null || pullRequest.base?.ref !== defaultBranch ||
      pullRequest.merge_commit_sha !== candidateSha) {
    fail("finalization Pull Requestがcandidate SHAとしてmainへmergeされていません");
  }
  const filenames = pullRequestFiles.map((file) => file.filename).sort();
  if (JSON.stringify(filenames) !== JSON.stringify(["release/intent.json"])) {
    fail("finalization Pull Requestはrelease/intent.jsonだけを変更する必要があります");
  }
  if (!Array.isArray(openPullRequests) || openPullRequests.length !== 0) {
    fail("main向けのopen Pull Requestが残っています");
  }
  const runs = successfulCandidateRuns.filter((run) =>
    run.head_branch === defaultBranch &&
    run.head_sha === candidateSha &&
    run.event === "push" &&
    run.status === "completed" &&
    run.conclusion === "success"
  );
  if (runs.length === 0) fail("同じSHAの成功済みmain release candidateがありません");
  if (tagExists) fail(`tag v${packageVersion}はすでに存在します`);
  if (releaseExists) fail(`Release v${packageVersion}はすでに存在します`);
  const runId = Math.max(...runs.map((run) => Number(run.id)).filter(Number.isSafeInteger));
  if (!Number.isSafeInteger(runId)) fail("main candidate run IDを決定できません");
  return { candidateSha, pullRequestNumber, runId, tag: `v${packageVersion}` };
}

function apiUrl(repository, path, query) {
  const url = new URL(`https://api.github.com/repos/${repository}/${path}`);
  for (const [name, value] of Object.entries(query ?? {})) url.searchParams.set(name, value);
  return url;
}

async function requestJson(repository, token, path, {
  allowMissing = false,
  fetchImpl = globalThis.fetch,
  query,
} = {}) {
  const response = await fetchImpl(apiUrl(repository, path, query), {
    headers: {
      Accept: "application/vnd.github+json",
      Authorization: `Bearer ${token}`,
      "X-GitHub-Api-Version": "2022-11-28",
    },
  });
  if (allowMissing && response.status === 404) return undefined;
  if (!response.ok) fail(`GitHub API ${path || "repository"} がHTTP ${response.status}を返しました`);
  return response.json();
}

async function requestPages(repository, token, path, query = {}, fetchImpl = globalThis.fetch) {
  const values = [];
  for (let page = 1; page <= MAX_API_PAGES; page += 1) {
    const result = await requestJson(repository, token, path, {
      fetchImpl,
      query: { ...query, page: String(page), per_page: "100" },
    });
    const pageValues = Array.isArray(result) ? result : result.workflow_runs;
    if (!Array.isArray(pageValues)) fail(`GitHub API ${path} のpage形式が不正です`);
    values.push(...pageValues);
    if (pageValues.length < 100) return values;
  }
  fail(`GitHub API ${path} がpage上限 ${MAX_API_PAGES}に達しました`);
}

function decodeContent(value, label) {
  if (value?.encoding !== "base64" || typeof value.content !== "string") {
    fail(`${label}をbase64 fileとして取得できません`);
  }
  return JSON.parse(Buffer.from(value.content.replaceAll("\n", ""), "base64").toString("utf8"));
}

export async function collectReadinessEvidence({
  repository,
  token,
  candidateSha,
  finalizationPullRequest,
  fetchImpl = globalThis.fetch,
  root = ROOT,
}) {
  const pullRequestNumber = positivePullRequestNumber(finalizationPullRequest);
  const repositoryState = await requestJson(repository, token, "", { fetchImpl });
  const defaultBranch = repositoryState.default_branch;
  const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", root), "utf8"));
  const [branch, candidateCommit, pullRequest, pullRequestFiles, openPullRequests, candidateRuns, tag, release] =
    await Promise.all([
      requestJson(repository, token, `commits/${defaultBranch}`, { fetchImpl }),
      requestJson(repository, token, `commits/${candidateSha}`, { fetchImpl }),
      requestJson(repository, token, `pulls/${pullRequestNumber}`, { fetchImpl }),
      requestPages(repository, token, `pulls/${pullRequestNumber}/files`, {}, fetchImpl),
      requestPages(repository, token, "pulls", { state: "open", base: defaultBranch }, fetchImpl),
      requestPages(repository, token, "actions/workflows/release.yml/runs", {
        branch: defaultBranch,
        event: "push",
        status: "success",
        head_sha: candidateSha,
      }, fetchImpl),
      requestJson(repository, token, `git/ref/tags/v${manifest.packageVersion}`, {
        allowMissing: true,
        fetchImpl,
      }),
      requestJson(repository, token, `releases/tags/v${manifest.packageVersion}`, {
        allowMissing: true,
        fetchImpl,
      }),
    ]);
  if (candidateCommit?.sha !== candidateSha || candidateCommit.parents?.length !== 1 ||
      !COMMIT_SHA.test(candidateCommit.parents[0]?.sha ?? "")) {
    fail("candidate commitは確認済みの単一親merge commitである必要があります");
  }
  const previousContent = await requestJson(
    repository,
    token,
    "contents/release/intent.json",
    { fetchImpl, query: { ref: candidateCommit.parents[0].sha } },
  );
  return {
    candidateSha,
    finalizationPullRequest: String(pullRequestNumber),
    defaultBranch,
    defaultBranchSha: branch.sha,
    intent: JSON.parse(readFileSync(new URL("release/intent.json", root), "utf8")),
    previousIntent: decodeContent(previousContent, "finalization前のrelease intent"),
    packageVersion: manifest.packageVersion,
    candidateCommit,
    pullRequest,
    pullRequestFiles,
    openPullRequests,
    successfulCandidateRuns: candidateRuns,
    tagExists: tag !== undefined,
    releaseExists: release !== undefined,
  };
}

async function main() {
  const repository = process.env.GITHUB_REPOSITORY;
  const token = process.env.GH_TOKEN;
  const candidateSha = process.env.CANDIDATE_SHA;
  const finalizationPullRequest = process.env.FINALIZATION_PR;
  if (!repository || !token || !candidateSha || !finalizationPullRequest) {
    fail("GITHUB_REPOSITORY、GH_TOKEN、CANDIDATE_SHAおよびFINALIZATION_PRが必要です");
  }
  const evidence = await collectReadinessEvidence({
    repository,
    token,
    candidateSha,
    finalizationPullRequest,
  });
  const result = validateReadinessEvidence(evidence);
  const output = process.env.GITHUB_OUTPUT;
  if (!output) fail("GITHUB_OUTPUTが必要です");
  const { appendFileSync } = await import("node:fs");
  appendFileSync(
    output,
    `candidate_sha=${result.candidateSha}\nrun_id=${result.runId}\ntag=${result.tag}\n`,
  );
  process.stdout.write(`release candidateを固定しました：${result.tag} ${result.candidateSha}\n`);
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  main().catch((error) => {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  });
}

import { readFileSync } from "node:fs";
import process from "node:process";

// Publication is guarded by the GitHub side: the `github-release` Environment
// restricts which branch may run the publish job, and the `v*` tag ruleset
// restricts tag creation. This script only pins the dispatched candidate to
// the current main tip, locates its successful candidate run, and refuses to
// publish a version that already has a tag or a Release.

const ROOT = new URL("../", import.meta.url);
const COMMIT_SHA = /^[0-9a-f]{40}$/;
const STABLE_TAG = /^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;

function fail(message) {
  throw new Error(message);
}

function apiUrl(repository, path, query) {
  // The repository itself is addressed with an empty path. Joining a separator
  // anyway leaves a trailing slash, which GitHub answers with 404.
  const suffix = path ? `/${path}` : "";
  const url = new URL(`https://api.github.com/repos/${repository}${suffix}`);
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

export async function assertTagAbsent({ repository, token, tag, fetchImpl = globalThis.fetch }) {
  if (!STABLE_TAG.test(tag)) fail("確認するstable tagが不正です");
  const existing = await requestJson(repository, token, `git/ref/tags/${tag}`, {
    allowMissing: true,
    fetchImpl,
  });
  if (existing !== undefined) fail(`tag ${tag}はすでに存在します`);
}

export async function resolveReleaseCandidate({
  repository,
  token,
  candidateSha,
  dispatchSha,
  fetchImpl = globalThis.fetch,
  root = ROOT,
}) {
  if (typeof candidateSha !== "string" || !COMMIT_SHA.test(candidateSha)) {
    fail("candidate SHAは小文字40文字のGit commitである必要があります");
  }
  if (dispatchSha !== candidateSha) {
    fail("candidate SHAは信頼済みmain dispatch SHAと一致する必要があります");
  }
  const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", root), "utf8"));
  const tag = `v${manifest.packageVersion}`;
  const [runs, existingTag, existingRelease] = await Promise.all([
    requestJson(repository, token, "actions/workflows/release.yml/runs", {
      fetchImpl,
      query: { branch: "main", event: "push", status: "success", head_sha: candidateSha },
    }),
    requestJson(repository, token, `git/ref/tags/${tag}`, { allowMissing: true, fetchImpl }),
    requestJson(repository, token, `releases/tags/${tag}`, { allowMissing: true, fetchImpl }),
  ]);
  if (existingTag !== undefined) fail(`tag ${tag}はすでに存在します`);
  if (existingRelease !== undefined) fail(`Release ${tag}はすでに存在します`);
  const candidates = (runs.workflow_runs ?? []).filter((run) =>
    run.head_sha === candidateSha && run.conclusion === "success"
  );
  if (candidates.length === 0) fail("同じSHAの成功済みmain release candidateがありません");
  const runId = Math.max(...candidates.map((run) => Number(run.id)).filter(Number.isSafeInteger));
  if (!Number.isSafeInteger(runId)) fail("main candidate run IDを決定できません");
  return { candidateSha, runId, tag };
}

async function main(args = process.argv.slice(2)) {
  const repository = process.env.GITHUB_REPOSITORY;
  const token = process.env.GH_TOKEN;
  if (args[0] === "--assert-tag-absent") {
    if (args.length !== 2 || !repository || !token) {
      fail("使用方法：node tools/release-readiness.mjs --assert-tag-absent vX.Y.Z");
    }
    await assertTagAbsent({ repository, token, tag: args[1] });
    process.stdout.write(`stable tagが存在しないことを確認しました：${args[1]}\n`);
    return;
  }
  if (args.length !== 0) {
    fail("使用方法：node tools/release-readiness.mjs");
  }
  const candidateSha = process.env.CANDIDATE_SHA;
  const dispatchSha = process.env.DISPATCH_SHA;
  if (!repository || !token || !candidateSha || !dispatchSha) {
    fail("GITHUB_REPOSITORY、GH_TOKEN、CANDIDATE_SHAおよびDISPATCH_SHAが必要です");
  }
  const result = await resolveReleaseCandidate({ repository, token, candidateSha, dispatchSha });
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

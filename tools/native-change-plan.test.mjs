import assert from "node:assert/strict";
import test from "node:test";
import distributionPlan from "../release/distribution-plan.json" with { type: "json" };
import {
  affectsGlobalCandidate,
  affectsNativeCandidate,
  auditCandidatePaths,
  candidateImpact,
  classifyCandidatePath,
  nativeChangePlan,
} from "./native-change-plan.mjs";

test("native archiveへ影響する入力だけを選択する", () => {
  for (const pathname of [
    "crates/adocweave/src/lib.rs",
    "crates/adocweave-cli/src/main.rs",
    "tools/native-release-smoke.mjs",
    ".github/workflows/release.yml",
    "Cargo.lock",
    "dist-workspace.toml",
    "LICENSE-MIT",
    "flake.nix",
  ]) {
    assert.equal(affectsNativeCandidate(pathname), true, pathname);
  }
  for (const pathname of [
    "editors/vscode/src/extension.ts",
    "web-worker/client.mjs",
    "tools/browser-release-smoke.mjs",
    "docs/user-guide/command-line.adoc",
    "fixtures/basic/input.adoc",
  ]) {
    assert.equal(affectsNativeCandidate(pathname), false, pathname);
  }
});

test("global archiveへ影響する入力だけを選択する", () => {
  for (const pathname of [
    "crates/adocweave/src/lib.rs",
    "crates/adocweave-wasm/src/lib.rs",
    "editors/vscode/src/extension.ts",
    "web-worker/client.mjs",
    "tools/browser-release-smoke.mjs",
    "tools/protocol-rust-codegen.mjs",
    "tools/protocol-rust-codegen.test.mjs",
    "release-manifest.json",
    "protocol/public-api.json",
  ]) {
    assert.equal(affectsGlobalCandidate(pathname), true, pathname);
  }
  for (const pathname of [
    "crates/adocweave-cli/src/main.rs",
    "crates/adocweave-lsp/src/main.rs",
    "tools/native-release-smoke.mjs",
    "docs/user-guide/command-line.adoc",
  ]) {
    assert.equal(affectsGlobalCandidate(pathname), false, pathname);
  }
});

test("未分類のsourceとbuild入力はfail-safeで両方のcandidateを要求する", () => {
  for (const pathname of [
    "crates/new-adapter/src/lib.rs",
    "new-build-system/config.json",
    "tools/new-release-helper.mjs",
    "tools/release-workflow-policy-helper.mjs",
  ]) {
    assert.deepEqual(candidateImpact(pathname), { global: true, native: true }, pathname);
    assert.equal(classifyCandidatePath(pathname).classified, false, pathname);
  }
});

test("tracked path監査は未分類pathを具体的に報告する", () => {
  const unknown = auditCandidatePaths([
    "docs/user-guide/command-line.adoc",
    "tools/host-executable.mjs",
    "new-build-system/config.json",
    "tools/new-release-helper.mjs",
  ]);
  assert.deepEqual(unknown, [
    "new-build-system/config.json",
    "tools/new-release-helper.mjs",
  ]);
});

test("Browser実行補助はglobalだけ、repository metadataはcandidate対象外に分類する", () => {
  for (const pathname of [
    "tools/browser-startup.mjs",
    "tools/browser-release-budget.mjs",
    "tools/browser-release-smoke.test.mjs",
    "tools/host-executable.mjs",
    "tools/host-executable.test.mjs",
  ]) {
    assert.deepEqual(candidateImpact(pathname), { global: true, native: false }, pathname);
  }
  for (const pathname of [
    ".github/dependabot.yml",
    ".github/dependabot-auto-merge-policy.json",
    ".github/pull_request_template.md",
    ".gitattributes",
    "deny.toml",
  ]) {
    assert.deepEqual(candidateImpact(pathname), { global: false, native: false }, pathname);
  }
});

test("成果物へ影響しない既知の文書とfixtureだけを明示的に除外する", () => {
  for (const pathname of [
    "CONTRIBUTING.adoc",
    "docs/developer-guide/architecture.adoc",
    "fixtures/basic/input.adoc",
    ".github/ISSUE_TEMPLATE/bug_report.yml",
    "tools/release-workflow-policy.mjs",
    "tools/release-workflow-policy.test.mjs",
    "tools/dependabot-auto-merge-policy.mjs",
    "tools/dependabot-auto-merge-policy.test.mjs",
    "tools/dependabot-auto-merge-workflow.test.mjs",
  ]) {
    assert.deepEqual(candidateImpact(pathname), { global: false, native: false }, pathname);
  }
});

test("crateへ埋め込むconformance manifestは両candidateへ含める", () => {
  const pathname = "crates/adocweave/conformance/cases.json";
  assert.deepEqual(candidateImpact(pathname), { global: true, native: true });
  const plan = nativeChangePlan("pull_request", [pathname], distributionPlan);
  assert.equal(plan.candidateRequired, true);
  assert.equal(plan.nativeRequired, true);
  assert.equal(plan.globalRequired, true);
});

test("dist設定はcommon、protocolはglobalだけに分類する", () => {
  assert.deepEqual(candidateImpact("dist-workspace.toml"), { global: true, native: true });
  assert.deepEqual(candidateImpact("protocol/public-api.json"), { global: true, native: false });
});

test("native pull requestではWindowsとmacOSだけを実OS検証する", () => {
  const plan = nativeChangePlan("pull_request", ["crates/adocweave-cli/src/main.rs"], distributionPlan);
  assert.equal(plan.candidateRequired, true);
  assert.equal(plan.nativeRequired, true);
  assert.equal(plan.globalRequired, false);
  assert.equal(plan.releaseMain, false);
  assert.equal(plan.preflightRequired, true);
  assert.deepEqual(plan.matrix.include.map(({ target, runner }) => ({ target, runner })), [
    { target: "aarch64-apple-darwin", runner: "macos-15" },
    { target: "x86_64-pc-windows-msvc", runner: "windows-2025" },
  ]);
});

test("global archiveだけのpull requestではnative buildを省略する", () => {
  const plan = nativeChangePlan("pull_request", ["editors/vscode/src/extension.ts"], distributionPlan);
  assert.equal(plan.candidateRequired, true);
  assert.equal(plan.nativeRequired, false);
  assert.equal(plan.globalRequired, true);
  assert.equal(plan.matrix.include.length, 2);
});

test("文書だけのpull requestではcandidate全体を省略する", () => {
  const plan = nativeChangePlan("pull_request", ["docs/user-guide/command-line.adoc"], distributionPlan);
  assert.equal(plan.candidateRequired, false);
  assert.equal(plan.nativeRequired, false);
  assert.equal(plan.globalRequired, false);
  assert.equal(plan.preflightRequired, false);
});

test("通常main pushではrelease candidateを構築しない", () => {
  const plan = nativeChangePlan(
    "push",
    ["crates/adocweave/src/lib.rs"],
    distributionPlan,
    "refs/heads/main",
  );
  assert.equal(plan.candidateRequired, false);
  assert.equal(plan.releaseMain, false);
});

test("未公開versionへ更新したmain pushでは全targetを検証する", () => {
  const plan = nativeChangePlan(
    "push",
    ["release-manifest.json", "Cargo.toml"],
    distributionPlan,
    "refs/heads/main",
    false,
  );
  assert.equal(plan.candidateRequired, true);
  assert.equal(plan.nativeRequired, true);
  assert.equal(plan.globalRequired, true);
  assert.equal(plan.releaseMain, true);
  assert.deepEqual(
    plan.matrix.include.map(({ target }) => target),
    distributionPlan.targets.map(({ triple }) => triple),
  );
});

test("未公開versionの修正main pushでもrelease candidateを再構築する", () => {
  const plan = nativeChangePlan(
    "push",
    ["crates/adocweave-cli/src/main.rs"],
    distributionPlan,
    "refs/heads/main",
    false,
  );
  assert.equal(plan.releaseMain, true);
  assert.equal(plan.candidateRequired, true);
  assert.equal(plan.matrix.include.length, distributionPlan.targets.length);
});

test("公開済みversionのmain pushではmanifest以外を変更してもcandidateを省略する", () => {
  const plan = nativeChangePlan(
    "push",
    ["crates/adocweave-cli/src/main.rs"],
    distributionPlan,
    "refs/heads/main",
    true,
  );
  assert.equal(plan.releaseMain, false);
  assert.equal(plan.candidateRequired, false);
});

test("version tagではmain candidateを再構築しない", () => {
  const plan = nativeChangePlan("push", [], distributionPlan, "refs/tags/v0.17.0");
  assert.equal(plan.candidateRequired, false);
  assert.equal(plan.preflightRequired, true);
});

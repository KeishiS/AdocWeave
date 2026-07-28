import assert from "node:assert/strict";
import test from "node:test";
import distributionPlan from "../release/distribution-plan.json" with { type: "json" };
import { affectsNativeCandidate, nativeChangePlan } from "./native-change-plan.mjs";

test("native成果物へ影響するsourceと配布設定を選択する", () => {
  for (const pathname of [
    "crates/adocweave/src/lib.rs",
    "tools/native-release-smoke.mjs",
    ".github/workflows/release.yml",
    "editors/vscode/src/extension.ts",
    "Cargo.lock",
    "flake.nix",
  ]) {
    assert.equal(affectsNativeCandidate(pathname), true, pathname);
  }
  assert.equal(affectsNativeCandidate("docs/user-guide/command-line.adoc"), false);
  assert.equal(affectsNativeCandidate("fixtures/basic/input.adoc"), false);
});

test("pull requestではWindowsとmacOSだけを実OS検証する", () => {
  const plan = nativeChangePlan("pull_request", ["crates/adocweave/src/lib.rs"], distributionPlan);
  assert.equal(plan.required, true);
  assert.deepEqual(plan.matrix.include.map(({ target, runner }) => ({ target, runner })), [
    { target: "aarch64-apple-darwin", runner: "macos-15" },
    { target: "x86_64-pc-windows-msvc", runner: "windows-2025" },
  ]);
});

test("文書だけのpull requestではnative jobを省略する", () => {
  const plan = nativeChangePlan("pull_request", ["docs/user-guide/command-line.adoc"], distributionPlan);
  assert.equal(plan.required, false);
  assert.equal(plan.matrix.include.length, 2);
});

test("main pushでは配布計画の全targetを検証する", () => {
  const plan = nativeChangePlan("push", [], distributionPlan, "refs/heads/main");
  assert.equal(plan.required, true);
  assert.deepEqual(
    plan.matrix.include.map(({ target }) => target),
    distributionPlan.targets.map(({ triple }) => triple),
  );
});

test("version tagではmain candidateを再構築しない", () => {
  const plan = nativeChangePlan("push", [], distributionPlan, "refs/tags/v0.14.0");
  assert.equal(plan.required, false);
});

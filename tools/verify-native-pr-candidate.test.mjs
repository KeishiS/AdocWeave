import assert from "node:assert/strict";
import test from "node:test";
import plan from "../release/distribution-plan.json" with { type: "json" };
import {
  expectedNativePullRequestAssets,
  verifyNativePullRequestAssets,
} from "./verify-native-pr-candidate.mjs";

test("pull request candidateはWindows・macOSとglobal成果物だけを要求する", () => {
  const expected = expectedNativePullRequestAssets(plan);
  assert.equal(expected.length, 7);
  assert(expected.includes("adocweave-cli-aarch64-apple-darwin.zip"));
  assert(expected.includes("adocweave-lsp-x86_64-pc-windows-msvc.zip"));
  assert(expected.includes(`adocweave-vscode-${plan.packageVersion}.vsix`));
  assert.equal(expected.some((name) => name.includes("unknown-linux-musl")), false);
});

test("pull request candidateの欠落と余分なfileを拒否する", () => {
  const expected = expectedNativePullRequestAssets(plan);
  assert.doesNotThrow(() => verifyNativePullRequestAssets(expected, plan));
  assert.throws(() => verifyNativePullRequestAssets(expected.slice(1), plan), /candidate mismatch/);
  assert.throws(() => verifyNativePullRequestAssets([...expected, "unknown.zip"], plan), /candidate mismatch/);
});

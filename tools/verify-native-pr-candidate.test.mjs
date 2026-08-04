import assert from "node:assert/strict";
import test from "node:test";
import plan from "../release/distribution-plan.json" with { type: "json" };
import {
  expectedPullRequestAssets,
  verifyPullRequestAssets,
} from "./verify-native-pr-candidate.mjs";

test("pull request candidateはWindows・macOSとglobal成果物だけを要求する", () => {
  const expected = expectedPullRequestAssets(plan);
  assert.equal(expected.length, 8);
  assert(expected.includes("adocweave-cli-aarch64-apple-darwin.zip"));
  assert(expected.includes("adocweave-lsp-x86_64-pc-windows-msvc.zip"));
  assert(expected.includes(`adocweave-vscode-${plan.packageVersion}.vsix`));
  assert.equal(expected.some((name) => name.includes("unknown-linux-musl")), false);
});

test("pull request candidateの欠落と余分なfileを拒否する", () => {
  const expected = expectedPullRequestAssets(plan);
  assert.doesNotThrow(() => verifyPullRequestAssets(expected, plan));
  assert.throws(() => verifyPullRequestAssets(expected.slice(1), plan), /candidate mismatch/);
  assert.throws(() => verifyPullRequestAssets([...expected, "unknown.zip"], plan), /candidate mismatch/);
});

test("native-only candidateはglobal成果物を要求しない", () => {
  const expected = expectedPullRequestAssets(plan, { global: false, native: true });
  assert.equal(expected.length, 4);
  assert.equal(expected.some((name) => name.includes("browser")), false);
  assert.doesNotThrow(() =>
    verifyPullRequestAssets(expected, plan, { global: false, native: true }));
});

test("global-only candidateはnative成果物を要求しない", () => {
  const expected = expectedPullRequestAssets(plan, { global: true, native: false });
  assert.deepEqual(expected, [
    `adocweave-browser-${plan.packageVersion}.tar.xz`,
    `adocweave-textlint-plugin-asciidoc-${plan.packageVersion}.tgz`,
    `adocweave-vscode-${plan.packageVersion}.vsix`,
    `adocweave-zed-${plan.packageVersion}.tar.xz`,
  ]);
  assert.doesNotThrow(() =>
    verifyPullRequestAssets(expected, plan, { global: true, native: false }));
});

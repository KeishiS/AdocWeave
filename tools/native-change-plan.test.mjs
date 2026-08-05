import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import distributionPlan from "../release/distribution-plan.json" with { type: "json" };
import {
  affectsGlobalCandidate,
  affectsNativeCandidate,
  auditCandidatePaths,
  candidateImpact,
  qualityScope,
  classifyCandidatePath,
  nativeChangePlan,
} from "./native-change-plan.mjs";

test("native archiveへ影響する入力だけを選択する", () => {
  for (const pathname of [
    "crates/adocweave/src/lib.rs",
    "crates/adocweave-cli/src/main.rs",
    "tools/native-lsp-smoke.mjs",
    "tools/native-lsp-smoke.test.mjs",
    "tools/native-release-smoke.mjs",
    ".github/workflows/release.yml",
    "Cargo.lock",
    "dist-workspace.toml",
    "release/version-sync.json",
    "LICENSE-MIT",
    "flake.nix",
  ]) {
    assert.equal(affectsNativeCandidate(pathname), true, pathname);
  }
  for (const pathname of [
    "editors/vscode/src/extension.ts",
    "web-worker/client.mjs",
    "tools/browser-release-smoke.mjs",
    "packages/textlint-plugin-asciidoc/processor.mjs",
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
    "crates/adocweave-textlint/src/lib.rs",
    "crates/adocweave-textlint-wasm/src/lib.rs",
    "editors/vscode/src/extension.ts",
    "web-worker/client.mjs",
    "tools/browser-release-smoke.mjs",
    "tools/package-textlint-plugin-release.sh",
    "tools/stage-textlint-plugin-package.mjs",
    "tools/textlint-plugin-package-contract.mjs",
    "tools/verify-textlint-plugin-package.mjs",
    "tools/verify-textlint-plugin-reproducibility.mjs",
    "tools/textlint-plugin-npx-smoke.mjs",
    "tools/textlint-plugin-post-release-smoke.mjs",
    "tools/textlint-plugin-e2e/package-lock.json",
    "tools/build-textlint-wasm-node.sh",
    "tools/textlint-plugin-compatibility-probe.mjs",
    "tools/textlint-plugin-compatibility-probe.test.mjs",
    "tools/verify-textlint-wasm-memory.mjs",
    "packages/textlint-plugin-asciidoc/processor.mjs",
    "tools/protocol-rust-codegen.mjs",
    "tools/protocol-rust-codegen.test.mjs",
    "tools/sync-release-version.mjs",
    "release-manifest.json",
    "protocol/public-api.json",
  ]) {
    assert.equal(affectsGlobalCandidate(pathname), true, pathname);
  }
  for (const pathname of [
    "crates/adocweave-cli/src/main.rs",
    "crates/adocweave-lsp/src/main.rs",
    "tools/native-lsp-smoke.mjs",
    "tools/native-lsp-smoke.test.mjs",
    "tools/native-release-smoke.mjs",
    "docs/user-guide/command-line.adoc",
  ]) {
    assert.equal(affectsGlobalCandidate(pathname), false, pathname);
  }
});

test("規約検査crateはcandidateへ影響しない", () => {
  // このcrateはlibraryもbinaryも持たず、testだけを持ちます。変更しても
  // release成果物に届かないため、candidateの再構築を要求しません。
  for (const pathname of [
    "crates/adocweave-governance/Cargo.toml",
    "crates/adocweave-governance/tests/repository_governance.rs",
  ]) {
    assert.deepEqual(candidateImpact(pathname), { global: false, native: false }, pathname);
    assert.equal(classifyCandidatePath(pathname).classified, true, pathname);
  }
  assert.deepEqual(auditCandidatePaths(["crates/adocweave-governance/Cargo.toml"]), []);
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
    "tools/dependabot-alert-inventory.sh",
    "tools/dependabot-alert-inventory.test.mjs",
    "tools/dependabot-alert-snapshot.sh",
    "tools/dependabot-alert-snapshot.test.mjs",
    "tools/dependabot-auto-merge-disable-result.jq",
    "tools/dependabot-auto-merge-pr-detail.jq",
    "tools/dependabot-auto-merge-pr-numbers.jq",
    "tools/dependabot-auto-merge-policy.mjs",
    "tools/dependabot-auto-merge-policy.test.mjs",
    "tools/dependabot-auto-merge-workflow.test.mjs",
    "tools/sync-release-version.test.mjs",
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

test("文書だけの変更ではRust sourceの検査を実行しない", () => {
  const scope = qualityScope(["docs/user-guide/command-line.adoc"]);

  assert.equal(scope.documents, true);
  assert.equal(scope.rustSource, false);
  assert.equal(scope.adapters, false);
  assert.equal(scope.fuzz, false);
  assert.equal(scope.nixPackage, false);
  assert.equal(scope.semver, false);
  assert.equal(scope.dependencies, false);
});

test("core crateの変更ではRust sourceとそれに依存する検査を実行する", () => {
  const scope = qualityScope(["crates/adocweave/src/html.rs"]);

  assert.equal(scope.rustSource, true);
  // fuzz、Nix packageおよびAPI差分検査はいずれもcore crateを構築します。
  assert.equal(scope.fuzz, true);
  assert.equal(scope.nixPackage, true);
  assert.equal(scope.semver, true);
  assert.equal(scope.documents, false);
});

test("VS Code拡張だけの変更ではRust sourceの検査を実行しない", () => {
  const scope = qualityScope(["editors/vscode/src/extension.ts"]);

  assert.equal(scope.adapters, true);
  assert.equal(scope.dependencies, true);
  assert.equal(scope.rustSource, false);
  assert.equal(scope.fuzz, false);
  assert.equal(scope.nixPackage, false);
});

test("公開textlint pluginの変更では文書、adapter、依存関係を検査する", () => {
  const scope = qualityScope(["packages/textlint-plugin-asciidoc/adapter.mjs"]);
  assert.equal(scope.documents, true);
  assert.equal(scope.adapters, true);
  assert.equal(scope.dependencies, true);
  assert.equal(scope.rustSource, false);
});

test("検査の定義を変える変更ではすべてを実行する", () => {
  // task graph、toolchainおよびworkflowは、どの検査の結果も変え得ます。
  for (const pathname of ["Makefile.toml", "flake.nix", ".github/workflows/quality.yml"]) {
    const scope = qualityScope([pathname]);
    assert.deepEqual(Object.values(scope), Array(Object.keys(scope).length).fill(true), pathname);
  }
});

test("互換性観測workflowの変更はpolicyとcandidate検証へ到達する", () => {
  const pathname = ".github/workflows/textlint-plugin-compatibility-probe.yml";
  assert.deepEqual(candidateImpact(pathname), { global: true, native: true });
  const scope = qualityScope([pathname]);
  assert.deepEqual(Object.values(scope), Array(Object.keys(scope).length).fill(true));
});

test("分類できないpathと空の変更集合ではすべてを実行する", () => {
  // 未分類のpathをすり抜けさせると、検証されないまま公開されます。
  const unknown = qualityScope(["brand-new-directory/thing.rs"]);
  assert.deepEqual(Object.values(unknown), Array(Object.keys(unknown).length).fill(true));

  const empty = qualityScope([]);
  assert.deepEqual(Object.values(empty), Array(Object.keys(empty).length).fill(true));
});

test("依存監査が読むすべての入力が監査を要求する", () => {
  // 監査の実行条件はRust source fileの集合が決めていました。その集合は
  // advisory revision、境界と例外の目録、重複のbaseline、監査script本体を
  // 含みません。それらを変えたPull Requestは、監査が受理する内容を変えた
  // うえで、監査を実行せずに成功していました。
  for (
    const pathname of [
      "security/rustsec-advisory-db-revision.txt",
      "security/dependency-boundaries.json",
      "security/dependency-exceptions.json",
      "security/duplicate-dependencies.json",
      "tools/dependency-governance.sh",
      "tools/verify-dependency-boundaries.mjs",
      "tools/verify-dependency-boundaries.test.mjs",
      "tools/verify-duplicate-dependencies.mjs",
      "tools/verify-vscode-dependencies.mjs",
      "tools/verify-vscode-dependencies.test.mjs",
      "tools/npm-lock-policy.mjs",
      "tools/verify-textlint-dependencies.mjs",
      "tools/verify-textlint-dependencies.test.mjs",
      "tools/generate-third-party-notices.mjs",
      "tools/package-textlint-plugin-release.sh",
      "tools/stage-textlint-plugin-package.mjs",
      "tools/textlint-plugin-package-contract.mjs",
      "tools/verify-textlint-plugin-package.mjs",
      "tools/verify-textlint-plugin-reproducibility.mjs",
      "tools/textlint-plugin-e2e/package-lock.json",
      "release/textlint-plugin-package-contract.json",
      "release/textlint-plugin-package-contract.schema.json",
      "Cargo.lock",
      "Cargo.toml",
      "deny.toml",
      "editors/zed/Cargo.lock",
      "editors/vscode/package-lock.json",
      "tools/textlint/package-lock.json",
    ]
  ) {
    assert.equal(qualityScope([pathname]).dependencies, true, pathname);
  }
});

test("監査scriptが名指しするrepository pathは監査の入力一覧に載る", () => {
  // 一覧とscriptはどちらも手書きです。scriptが新しいfileを読み始めても、
  // 一覧へ加え忘れれば、そのfileの変更は監査を素通りします。この検査は
  // scriptの本文からpathを読み、一覧との差を報告します。
  const script = readFileSync(new URL("dependency-governance.sh", import.meta.url), "utf8");
  const referenced = new Set(
    [...script.matchAll(/(?:^|[\s"'`(])((?:security|tools)\/[A-Za-z0-9._/-]+)/g)]
      .map((match) => match[1]),
  );
  assert.notEqual(referenced.size, 0, "監査scriptからpathを読み取れません");

  const missing = [...referenced].filter((pathname) =>
    !qualityScope([pathname]).dependencies
  );
  assert.deepEqual(missing, [], `監査の入力一覧に無いpath: ${missing.join(", ")}`);
});

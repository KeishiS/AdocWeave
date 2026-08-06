import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

import {
  loadBreakingRustApi,
  validateBreakingRustApi,
} from "./breaking-rust-api.mjs";

import {
  CHECKED_CRATES,
  CRATE_INTRODUCTIONS,
  UNCHECKED_CRATES,
  baselineTag,
  checkedCrates,
  comparisonReleaseType,
  cratesForBaseline,
  finishedCrates,
  parseVersion,
  requireRecordedBreakingChanges,
  releaseStep,
  reportedFailureBlocks,
  reportedFailures,
  verifySemverResult,
} from "./semver-gate.mjs";

const ROOT = new URL("../", import.meta.url);
const workspace = readFileSync(new URL("Cargo.toml", ROOT), "utf8");
const releaseManifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));

test("workspace memberはすべて検査対象か、理由付きの対象外である", () => {
  const members = [...workspace.matchAll(/^\s*"crates\/([\w-]+)",$/gm)].map((match) => match[1]);
  const covered = [...CHECKED_CRATES, ...Object.keys(UNCHECKED_CRATES)];

  assert.deepEqual(covered.sort(), members.sort());
  // 対象外は理由を伴います。書き忘れで検査から外れることを防ぎます。
  for (const [name, reason] of Object.entries(UNCHECKED_CRATES)) {
    assert.ok(reason.length > 0, `${name}に対象外の理由がありません`);
    assert.ok(!CHECKED_CRATES.includes(name), `${name}が対象と対象外の両方にあります`);
  }
});

test("library targetを持つcrateはすべて検査対象である", () => {
  const members = [...workspace.matchAll(/^\s*"crates\/([\w-]+)",$/gm)].map((match) => match[1]);
  // Rust APIはlibrary targetにしかないため、それが対象と対象外を分ける基準です。
  const withLibrary = members.filter((name) =>
    existsSync(new URL(`crates/${name}/src/lib.rs`, ROOT)),
  );

  assert.deepEqual([...CHECKED_CRATES].sort(), withLibrary.sort());
  for (const [name, version] of Object.entries(CRATE_INTRODUCTIONS)) {
    assert.ok(CHECKED_CRATES.includes(name), `${name}が公開API検査の一覧にありません`);
    assert.deepEqual(Object.keys(version).sort(), ["major", "minor", "patch"]);
    assert.ok(Object.values(version).every(Number.isInteger));
  }
});

test("新規crateの比較省略は、そのcrateを含むbaselineで自動的に失効する", () => {
  const beforeIntroduction = cratesForBaseline(parseVersion("0.27.3", "baseline"));
  const afterWasmIntroduction = cratesForBaseline({ major: 0, minor: 29, patch: 0 });
  const afterAdapterIntroduction = cratesForBaseline({ major: 0, minor: 30, patch: 0 });

  assert.ok(!beforeIntroduction.includes("adocweave-textlint-wasm"));
  assert.ok(!beforeIntroduction.includes("adocweave-textlint"));
  assert.ok(afterWasmIntroduction.includes("adocweave-textlint-wasm"));
  assert.ok(!afterWasmIntroduction.includes("adocweave-textlint"));
  assert.ok(afterAdapterIntroduction.includes("adocweave-textlint"));
  assert.ok(beforeIntroduction.includes("adocweave"));
});

test("release種別はbaselineと候補のversionから決まる", () => {
  const step = (baseline, candidate) =>
    releaseStep(parseVersion(baseline, "baseline"), parseVersion(candidate, "候補"));

  assert.equal(step("0.4.0", "0.4.1"), "patch");
  // 0.yではminorの位置が破壊的変更を運びます。
  assert.equal(step("0.4.0", "0.5.0"), "minor");
  assert.equal(step("0.4.0", "1.0.0"), "major");
  assert.equal(comparisonReleaseType("patch"), "patch");
  assert.equal(comparisonReleaseType("minor"), "minor");
  assert.equal(comparisonReleaseType("major"), "minor");
});

test("後戻りするversionと不正な形式は失敗する", () => {
  const step = (baseline, candidate) =>
    releaseStep(parseVersion(baseline, "baseline"), parseVersion(candidate, "候補"));

  assert.throws(() => step("0.4.1", "0.4.0"), /baseline以下/);
  assert.throws(() => step("0.4.0", "0.4.0"), /baseline以下/);
  assert.throws(() => step("0.5.0", "0.4.9"), /古いminor版/);
  assert.throws(() => step("1.0.0", "0.9.0"), /古いmajor版/);
  assert.throws(() => parseVersion("v0.4.0", "候補"), /X\.Y\.Z/);
  assert.throws(() => parseVersion("0.4.0-rc.1", "候補"), /X\.Y\.Z/);
});

test("baselineは候補より前の最新stable tagを選ぶ", () => {
  const tags = ["v0.2.0", "v0.4.0", "v0.3.0"];
  const candidate = parseVersion("0.5.0", "候補");

  assert.equal(baselineTag(tags, candidate).tag, "v0.4.0");
  // 数値として比較するため、文字列順では最大になるv0.9.0を選びません。
  assert.equal(
    baselineTag(["v0.9.0", "v0.10.0"], parseVersion("0.11.0", "候補")).tag,
    "v0.10.0",
  );
  // 候補と同じか新しいtagは、自分自身との比較になるため除きます。
  assert.equal(baselineTag([...tags, "v0.5.0", "v0.6.0"], candidate).tag, "v0.4.0");
});

test("baselineが無い場合はfail openせず失敗する", () => {
  const candidate = parseVersion("0.5.0", "候補");

  assert.throws(() => baselineTag([], candidate), /stable tagがありません/);
  assert.throws(() => baselineTag(["v0.5.0", "v1.0.0"], candidate), /stable tagがありません/);
  // release trainが受け付けないtagはbaselineにしません。
  assert.throws(() => baselineTag(["v0.4.0-rc.1", "nightly"], candidate), /stable tagがありません/);
});

test("比較したcrateと破壊的変更をそれぞれの出力から読み取る", () => {
  const diagnostics = [
    "--- failure function_missing: pub fn removed or renamed ---",
    "",
    "Description:",
    "A publicly-visible function cannot be imported by its prior path.",
    "       Failed in:",
    "  function adocweave::parse in /checkout/crates/adocweave/src/lib.rs:48",
    "  function adocweave::compile in C:\\checkout\\crates\\adocweave\\src\\lib.rs:52:7",
  ].join("\n");
  const progress = [
    "    Checking adocweave v0.4.0 -> v0.5.0 (major change)",
    "     Summary semver requires new major version: 1 major and 0 minor checks failed",
    "    Finished [   1.407s] adocweave",
    "    Checking adocweave-config v0.4.0 -> v0.5.0 (major change)",
    "     Summary no semver update required",
    "    Finished [   0.300s] adocweave-config",
  ].join("\n");

  assert.deepEqual(checkedCrates(progress), ["adocweave", "adocweave-config"]);
  assert.deepEqual(finishedCrates(progress), ["adocweave", "adocweave-config"]);
  assert.deepEqual(reportedFailures(diagnostics), [
    {
      crate: "adocweave",
      lint: "function_missing",
      summary: "pub fn removed or renamed",
      item: "function adocweave::parse",
    },
    {
      crate: "adocweave",
      lint: "function_missing",
      summary: "pub fn removed or renamed",
      item: "function adocweave::compile",
    },
  ]);
});

test("何も比較しなかった出力からはcrateを読み取らない", () => {
  // publish = false のcrateだけのworkspaceでは、--workspaceがこの出力で成功します。
  assert.deepEqual(checkedCrates("     Cloning v0.4.0\n"), []);
  assert.deepEqual(reportedFailures("     Summary no semver update required\n"), []);
});

test("固定したcargo-semver-checks 0.48.0のfailureをblockとして解析する", () => {
  const output = [
    "--- failure constructible_struct_adds_field: externally-constructible struct adds field ---",
    "",
    "Failed in:",
    "  field ResolvedProjectConfig.workspace in /checkout/crates/adocweave-config/src/lib.rs:641",
  ].join("\n");

  assert.deepEqual(reportedFailureBlocks(output), [{
    lint: "constructible_struct_adds_field",
    summary: "externally-constructible struct adds field",
    failedIn: true,
    items: [{ crate: "adocweave-config", item: "field ResolvedProjectConfig.workspace" }],
  }]);
  assert.deepEqual(reportedFailures(output), [{
    crate: "adocweave-config",
    lint: "constructible_struct_adds_field",
    summary: "externally-constructible struct adds field",
    item: "field ResolvedProjectConfig.workspace",
  }]);
});

test("failure blockの一部でも解析できなければ全体を拒否する", () => {
  const first = [
    "--- failure function_missing: pub fn removed or renamed ---",
    "Failed in:",
    "  function adocweave::parse in /checkout/crates/adocweave/src/lib.rs:48",
  ];
  assert.throws(
    () => reportedFailures([...first, "--- failure second heading has no summary ---"].join("\n")),
    /failure見出しを解析できません/,
  );
  assert.throws(
    () => reportedFailures([...first, "  a future item representation"].join("\n")),
    /解析できない対象/,
  );
  assert.throws(
    () => reportedFailures("--- failure function_missing: removed ---\nDescription only"),
    /Failed inがありません/,
  );
});

const candidate = parseVersion(releaseManifest.packageVersion, "候補");
const detectedChange = {
  crate: "adocweave-config",
  lint: "constructible_struct_adds_field",
  item: "field ResolvedProjectConfig.workspace",
  summary: "externally-constructible struct adds field",
};
const recordedChange = {
  ...detectedChange,
  description: "ResolvedProjectConfigにworkspaceを追加しました。",
  migration: "構造体リテラルへworkspaceを追加します。",
};
const record = (changes = [recordedChange]) => ({
  schemaVersion: 1,
  releaseVersion: releaseManifest.packageVersion,
  changes,
});

test("破壊的変更記録はschemaと未知項目を厳密に検査する", () => {
  assert.deepEqual(validateBreakingRustApi(record()), record());
  const actual = loadBreakingRustApi();
  assert.equal(actual.releaseVersion, releaseManifest.packageVersion);
  assert.deepEqual(
    actual.changes.map(({ crate, lint, item }) => ({ crate, lint, item })),
    [{ crate: "adocweave-host", lint: "enum_variant_added", item: "variant ResourceError:Job" }],
  );
  assert.deepEqual(validateBreakingRustApi(record([])).changes, []);
  assert.throws(() => validateBreakingRustApi({ ...record(), extra: true }), /未知または不足/);
  assert.throws(
    () => validateBreakingRustApi(record([{ ...recordedChange, extra: "unexpected" }])),
    /未知または不足/,
  );
  assert.throws(
    () => validateBreakingRustApi(record([{ ...recordedChange, migration: "" }])),
    /migrationが空/,
  );
  assert.throws(() => validateBreakingRustApi(record([recordedChange, recordedChange])), /重複/);
});

const verify = (overrides) =>
  verifySemverResult({
    candidate,
    step: "minor",
    status: 1,
    signal: null,
    failures: [detectedChange],
    record: record(),
    expectedCrates: ["adocweave-config"],
    completedCrates: ["adocweave-config"],
    ...overrides,
  });

test("minorとmajorは検出結果に完全一致する記録だけを許容する", () => {
  assert.doesNotThrow(() => verify());
  assert.doesNotThrow(() => verify({ step: "major" }));
  assert.throws(
    () =>
      requireRecordedBreakingChanges(
        candidate,
        [detectedChange, { ...detectedChange, item: "field ResolvedProjectConfig.other" }],
        record(),
      ),
    /未記録 1 件/,
  );
  assert.throws(
    () =>
      requireRecordedBreakingChanges(
        candidate,
        [detectedChange],
        record([recordedChange, { ...recordedChange, item: "field ResolvedProjectConfig.other" }]),
      ),
    /余分 1 件/,
  );
  assert.throws(
    () =>
      requireRecordedBreakingChanges(
        candidate,
        [detectedChange],
        record([{ ...recordedChange, summary: "異なる説明" }]),
      ),
    /summary/,
  );
});

test("patchの破壊的変更とツールの異常終了を区別して拒否する", () => {
  assert.throws(
    () => verify({ step: "patch" }),
    /patch releaseに破壊的変更/,
  );
  assert.throws(
    () => verify({ status: 2 }),
    /終了ステータスが不正/,
  );
  assert.throws(
    () => verify({ status: 0 }),
    /終了ステータスが不正/,
  );
  for (const status of [101, null]) {
    assert.throws(() => verify({ status }), /終了ステータスが不正/);
  }
  assert.throws(() => verify({ signal: "SIGTERM", status: null }), /signal SIGTERM/);
  assert.doesNotThrow(() =>
    verify({ step: "patch", status: 0, failures: [], record: record([]) }),
  );
  const interruptedOutput = [
    `    Checking adocweave-config v0.30.1 -> v${releaseManifest.packageVersion} (assume minor change)`,
    "error: failed to build the next package",
  ].join("\n");
  assert.throws(
    () =>
      verify({
        completedCrates: finishedCrates(interruptedOutput),
      }),
    /比較を完了せず、異常終了/,
  );
});

test("変更ゼロでも古い記録を受理しない", () => {
  assert.throws(
    () => verify({ status: 0, failures: [], record: record() }),
    /余分 1 件/,
  );
  assert.throws(
    () => verify({ status: 0, failures: [], record: { ...record([]), releaseVersion: "0.30.1" } }),
    /releaseVersionが候補と一致しません/,
  );
});

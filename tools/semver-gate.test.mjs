import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

import {
  CHECKED_CRATES,
  CRATE_INTRODUCTIONS,
  UNCHECKED_CRATES,
  baselineTag,
  checkedCrates,
  cratesForBaseline,
  parseVersion,
  releaseStep,
  reportedFailures,
} from "./semver-gate.mjs";

const ROOT = new URL("../", import.meta.url);
const workspace = readFileSync(new URL("Cargo.toml", ROOT), "utf8");

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

test("比較したcrateと破壊的変更を出力から読み取る", () => {
  const output = [
    "    Checking adocweave v0.4.0 -> v0.5.0 (major change)",
    "--- failure function_missing: pub fn removed or renamed ---",
    "",
    "Description:",
    "A publicly-visible function cannot be imported by its prior path.",
    "     Summary semver requires new major version: 1 major and 0 minor checks failed",
    "    Checking adocweave-config v0.4.0 -> v0.5.0 (major change)",
    "     Summary no semver update required",
  ].join("\n");

  assert.deepEqual(checkedCrates(output), ["adocweave", "adocweave-config"]);
  assert.deepEqual(reportedFailures(output), [
    { lint: "function_missing", summary: "pub fn removed or renamed" },
  ]);
});

test("何も比較しなかった出力からはcrateを読み取らない", () => {
  // publish = false のcrateだけのworkspaceでは、--workspaceがこの出力で成功します。
  assert.deepEqual(checkedCrates("     Cloning v0.4.0\n"), []);
  assert.deepEqual(reportedFailures("     Summary no semver update required\n"), []);
});

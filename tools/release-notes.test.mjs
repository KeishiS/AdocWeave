import assert from "node:assert/strict";
import test from "node:test";

import {
  RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION,
  RELEASE_NOTES_VERSION,
  buildReleaseNotes,
  validateReleaseNotes,
} from "./release-notes.mjs";
import manifest from "../release-manifest.json" with { type: "json" };
import protocol from "../protocol/public-api.json" with { type: "json" };

test("Release Notesはv0.17.1の修正と受入契約を含む", () => {
  const notes = buildReleaseNotes(`v${RELEASE_NOTES_VERSION}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /## 主な変更/);
  assert.match(notes, /CST（入力を失わず保持する構文木）/);
  assert.match(notes, /`SyntaxTree::reconstruct\(\)`/);
  assert.match(notes, /byte単位で復元/);
  assert.match(notes, /入れ子になった未閉じdelimiter/);
  assert.match(notes, /親block内へ制限/);
  assert.match(notes, /`InternalInvariant`/);
  assert.match(notes, /`unclosed-block`診断/);
  assert.match(notes, /HTTP 200と対象文書の応答/);
  assert.match(notes, /製品のCLI動作は変更していません/);
  assert.match(notes, /## 内部品質の改善/);
  assert.match(notes, /Dependabot自動mergeのpolicyは停止状態/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /v0\.17\.0からschema、公開API、CLI引数、診断codeおよびHTML契約を変更していません/);
  assert.match(notes, /誤ったnode順、重複範囲または親blockを越えた範囲に依存するsnapshotは更新が必要/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /gh attestation verify/);
  assert.match(notes, /`--version --json`が`0\.17\.1`を返す/);
  assert.match(notes, /以前のVSIXとnative directoryを保持/);
  assert.match(notes, /registryへpackageまたは拡張を公開しません/);
  assert.match(notes, /Developer ID署名とnotarizationを行わず/);
  assert.match(notes, /Authenticode署名を行いません/);
  assert.match(
    notes,
    new RegExp(`WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}`),
  );
  assert.match(notes, new RegExp(`Worker protocol version：${protocol.workerProtocolVersion}`));
  assert.match(notes, new RegExp(`統一package version：${RELEASE_NOTES_VERSION}`));
  assert.match(notes, new RegExp(`release manifest schema version：${manifest.schemaVersion}`));
  assert.match(notes, new RegExp(`対応Rust toolchain：${manifest.rustVersion}`));
});

test("Release Notesは別release trainのtagを拒否する", () => {
  assert.equal(manifest.packageVersion, RELEASE_NOTES_VERSION);
  assert.throws(() => buildReleaseNotes("v0.17.0"), /v0\.17\.1専用/);
  assert.throws(() => buildReleaseNotes("v9.9.9"), /v0\.17\.1専用/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION,
  RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION,
  RELEASE_NOTES_VERSION,
  buildReleaseNotes,
  validateReleaseNotes,
} from "./release-notes.mjs";
import manifest from "../release-manifest.json" with { type: "json" };
import protocol from "../protocol/public-api.json" with { type: "json" };

test(`Release Notesはv${RELEASE_NOTES_VERSION}の変更内容と移行方法を含む`, () => {
  const notes = buildReleaseNotes(`v${RELEASE_NOTES_VERSION}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /## 主な変更/);
  assert.match(notes, /1打鍵あたり26ミリ秒から10\.6ミリ秒になります/);
  assert.match(notes, /要求へ応答するthreadの外へ移しました/);
  assert.match(notes, /Renameを始める前に答えるようにしました/);
  assert.match(notes, /``nodeVersion``だけで決めるようにしました/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /WASM protocol schema version/);
  assert.match(notes, /v0\.23\.0から変更していません/);
  assert.match(notes, /schema versionを3から4へ更新しました/);
  assert.match(notes, /``prepareProvider``を宣言します/);
  // 直前のreleaseのRelease Notesが、行っていないmanifestの変更を告知しました。
  // 公開済みの本文は書き換えず、ここで訂正します。
  assert.match(notes, /訂正：v0\.25\.0のRelease Notesは/);
  assert.match(notes, /変更を行ったのはこのreleaseです/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /設定の移行は不要です/);
  assert.match(notes, /``schemaVersion``が4になり``nodeVersion``が加わります/);
  assert.match(notes, /アンカー定義の上でだけRenameを始められます/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /gh attestation verify/);
  assert.match(
    notes,
    new RegExp(`\`--version --json\`が\`${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}\`を返す`),
  );
  assert.match(notes, /以前のVSIXとnative directoryを保持/);
  assert.match(notes, /rollback時は旧directoryをdev extensionとして選び直し、Zedを再起動/);
  assert.match(notes, /registryへpackageまたは拡張を公開しません/);
  assert.match(notes, /Developer ID署名とnotarizationを行わず/);
  assert.match(notes, /Authenticode署名を行いません/);
  assert.match(notes, /引用表示の組み立ては利用側アプリの責務です/);
  assert.match(notes, /解決結果を渡さない引用の表示は`unresolved_references`の設定に従い/);
  assert.match(notes, /複数ファイルの解析にはディレクトリのworkspace folderが必要/);
  assert.match(notes, /要求へ応答するthreadの外で行います/);
  assert.match(
    notes,
    new RegExp(`WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}`),
  );
  assert.match(notes, new RegExp(`Worker protocol version：${protocol.workerProtocolVersion}`));
  assert.match(notes, new RegExp(`統一package version：${RELEASE_NOTES_VERSION}`));
  assert.match(notes, new RegExp(`release manifest schema version：${manifest.schemaVersion}`));
  assert.match(notes, new RegExp(`対応Rust toolchain：${manifest.rustVersion}`));
});

test("Release Notesが述べるschema versionはmanifestの実際の値と一致する", () => {
  const notes = buildReleaseNotes(`v${RELEASE_NOTES_VERSION}`);

  // 過去のReleaseで、行っていないmanifestの変更を告知したことがあります。同じ本文が
  // 一方で現在のschema versionを述べ、他方で別の遷移を述べていました。本文に現れる
  // 遷移の到達値は、必ずmanifestの現在値と一致します。
  const transitions = [...notes.matchAll(/schema versionを(\d+)から(\d+)へ/g)];
  assert.notEqual(transitions.length, 0, "schema versionの遷移が本文にありません");
  for (const [, from, to] of transitions) {
    assert.equal(Number(from), PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION);
    assert.equal(Number(to), manifest.schemaVersion);
  }
  assert.equal(manifest.schemaVersion, PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION + 1);
  for (const [, value] of notes.matchAll(/``schemaVersion``が(\d+)になり/g)) {
    assert.equal(Number(value), manifest.schemaVersion);
  }
});

test("Release Notesは別release trainのtagを拒否する", () => {
  assert.equal(manifest.packageVersion, RELEASE_NOTES_VERSION);
  assert.throws(() => buildReleaseNotes("v0.25.0"), /v0\.26\.0専用/);
  assert.throws(() => buildReleaseNotes("v9.9.9"), /v0\.26\.0専用/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

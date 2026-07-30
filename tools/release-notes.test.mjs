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

test(`Release Notesはv${RELEASE_NOTES_VERSION}の変更内容と移行方法を含む`, () => {
  const notes = buildReleaseNotes(`v${RELEASE_NOTES_VERSION}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /## 主な変更/);
  assert.match(notes, /key集合を、すべての実行経路で同じにしました/);
  assert.match(notes, /`sourceId`、`related`および`fixes`が常に存在します/);
  assert.match(notes, /trapしたworkerを次の要求へ持ち越さない/);
  assert.match(notes, /設定schemaに破壊的変更はありません/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /WASM protocol schema version/);
  assert.match(notes, /v0\.20\.1から変更していません/);
  assert.match(notes, /破壊的変更：`check --format json`の出力形式を変更しました/);
  assert.match(notes, /`wasm-trapped`というcodeで通知します/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /設定の移行は不要です/);
  assert.match(notes, /keyの有無で分岐している処理は、分岐を削除してください/);
  assert.match(notes, /keyの順序に依存する処理と、出力文字列をそのまま比較しているtest/);
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
  assert.match(notes, /今回の変更は`--format json`だけが対象です/);
  assert.match(notes, /native CLIとLanguage Serverの診断codeは変更していません/);
  assert.match(notes, /複数ファイルの解析にはディレクトリのworkspace folderが必要/);
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
  assert.throws(() => buildReleaseNotes("v0.20.1"), /v0\.21\.0専用/);
  assert.throws(() => buildReleaseNotes("v9.9.9"), /v0\.21\.0専用/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

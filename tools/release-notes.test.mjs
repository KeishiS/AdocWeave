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
  assert.match(notes, /引用情報を公開projectionとWASM protocolへ公開しました/);
  assert.match(notes, /解決した引用表示をHTMLへ渡せるようにしました/);
  assert.match(notes, /断片ごとに参照先anchorを指定できます/);
  assert.match(notes, /その項目へlinkし、項目側の戻りlinkにも並びます/);
  assert.match(notes, /解決できるはずのlocal targetを検査不能として報告することがあった問題を修正しました/);
  assert.match(notes, /前処理の入口を6変種から整理し/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /WASM protocol schema version/);
  assert.match(notes, /schema versionをv0\.22\.0の7から9へ更新しました/);
  assert.match(notes, /`RenderInputs::new`を削除しました/);
  assert.match(notes, /`EffectiveProcessingOptions::preprocess_and_analyze`へ統合しています/);
  assert.match(notes, /診断code`unknown-citation-anchor`を追加しました/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /設定の移行は不要です/);
  assert.match(notes, /`RenderInputs::default\(\)\.with_references\(references\)\.with_resources\(resources\)`へ置き換えてください/);
  assert.match(notes, /macro全体のrangeで解決結果を渡してください/);
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
  assert.throws(() => buildReleaseNotes("v0.22.0"), /v0\.23\.0専用/);
  assert.throws(() => buildReleaseNotes("v9.9.9"), /v0\.23\.0専用/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

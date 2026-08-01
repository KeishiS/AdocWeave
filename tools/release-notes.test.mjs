import assert from "node:assert/strict";
import test from "node:test";

import {
  PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION,
  PREVIOUS_RELEASE_VERSION,
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
  assert.match(
    notes,
    new RegExp(`v${PREVIOUS_RELEASE_VERSION.replaceAll(".", "\\.")}のRelease Notesで設定schemaに変更がないと案内していた誤り`),
  );
  assert.match(notes, /``resources\.roots``と``local-targets\.project-root``へ相対パスの制約/);
  assert.match(notes, /``local-targets\.enabled``が``true``の場合は``project-root``を必須/);
  assert.match(notes, /属性名の大文字と小文字を区別しなくなりました/);
  assert.match(notes, /上限を重複して消費しなくなりました/);
  assert.match(notes, /``wasm-trapped``を追加しました/);
  assert.match(notes, /Content-Lengthを返さない配信/);
  assert.match(notes, /作成した処理だけが削除する/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /WASM protocol schema version/);
  assert.match(notes, /v0\.23\.0から変更していません/);
  assert.match(notes, /schema versionは4のままで、項目を追加も削除もしていません/);
  // patch版のため、semver gateは破壊的変更を受理しません。本文も述べません。
  assert.doesNotMatch(notes, /破壊的変更：/);
  assert.match(notes, /破壊的変更はありません/);
  // 挙動が変わるreleaseでは、何がどう変わるかを本文が述べます。
  assert.match(notes, /挙動の変更：``ifdef``/);
  assert.match(notes, /挙動の変更：存在しないパス/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /実行時に受理されていた設定の移行は不要です/);
  assert.doesNotMatch(notes, /設定schemaはv0\.27\.1から変更していません/);
  assert.match(notes, /小文字の綴りで書いた文書の結果は変わりません/);
  assert.match(notes, /``wasm-trapped``の分岐を加えてください/);
  assert.match(notes, /``schemaVersion``は4のままです/);
  assert.match(notes, /バージョンの異なる配布物を混ぜて使えない/);
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
  // 一方で現在のschema versionを述べ、他方で別の遷移を述べていました。遷移を述べるのは
  // 実際に値が変わったreleaseだけとし、到達値は必ずmanifestの現在値と一致させます。
  const transitions = [...notes.matchAll(/schema versionを(\d+)から(\d+)へ/g)];
  if (manifest.schemaVersion === PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION) {
    assert.equal(transitions.length, 0, "変更していないschema versionの遷移を述べています");
    assert.match(notes, new RegExp(`schema versionは${manifest.schemaVersion}のまま`));
  } else {
    assert.notEqual(transitions.length, 0, "schema versionの遷移が本文にありません");
    for (const [, from, to] of transitions) {
      assert.equal(Number(from), PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION);
      assert.equal(Number(to), manifest.schemaVersion);
    }
  }
  assert.ok(
    manifest.schemaVersion >= PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION,
    "manifestのschema versionが直前のreleaseより小さくなっています",
  );
  for (const [, value] of notes.matchAll(/``schemaVersion``は(\d+)のまま/g)) {
    assert.equal(Number(value), manifest.schemaVersion);
  }
});

test("Release Notesは別release trainのtagを拒否する", () => {
  assert.equal(manifest.packageVersion, RELEASE_NOTES_VERSION);
  assert.throws(() => buildReleaseNotes("v0.27.1"), /v0\.27\.2専用/);
  assert.throws(() => buildReleaseNotes("v9.9.9"), /v0\.27\.2専用/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  PREVIOUS_RELEASE_MANIFEST_SCHEMA_VERSION,
  PREVIOUS_RELEASE_VERSION,
  RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION,
  RELEASE_NOTES_VERSION,
  breakingContractNotes,
  breakingMigrationNotes,
  buildReleaseNotes,
  validateReleaseNotes,
} from "./release-notes.mjs";
import manifest from "../release-manifest.json" with { type: "json" };
import protocol from "../protocol/public-api.json" with { type: "json" };
import { loadTextlintPluginPackageContract } from "./textlint-plugin-package-contract.mjs";

const textlintContract = loadTextlintPluginPackageContract();

test("textlint対応情報をpackage contractからRelease Notesへ投影する", () => {
  const source = readFileSync(new URL("./release-notes.mjs", import.meta.url), "utf8");
  assert.match(source, /loadTextlintPluginPackageContract/);
  for (const value of [
    textlintContract.identity.packageName,
    textlintContract.identity.pluginName,
    textlintContract.compatibility.nodeEngine,
    textlintContract.compatibility.textlintVersion,
  ]) {
    assert.equal(source.includes(value), false, `Release Notesに契約値を直書きしています：${value}`);
  }
});

test(`Release Notesはv${RELEASE_NOTES_VERSION}の変更内容と移行方法を含む`, () => {
  const notes = buildReleaseNotes(`v${RELEASE_NOTES_VERSION}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /## 主な変更/);
  assert.equal(PREVIOUS_RELEASE_VERSION, "0.34.0");
  assert.match(notes, /#515：/);
  assert.match(notes, /空白を出力せず行をつなぐ/);
  assert.match(notes, /#516：/);
  assert.match(notes, /inline macroは直前に空白がなければ認識されません/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /WASM protocol schema version/);
  assert.match(notes, /どちらもv0\.34\.0から変更していません/);
  assert.match(notes, /schema versionは4のままで、項目を追加も削除もしていません/);
  assert.match(notes, /Rust APIの破壊的変更：ありません/);
  assert.match(notes, /公開Rust APIは変更していません/);
  assert.match(notes, /HTML出力を変更しました/);
  assert.match(notes, /macroの直後の空白と、書式記号の前後の空白は従来どおり出力します/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /0\.35\.0のpackageとAPIへ更新/);
  assert.match(notes, /requestの``packageVersion``も``0\.35\.0``にそろえて/);
  assert.match(notes, /``schemaVersion``はrequestの項目ではありません/);
  assert.match(notes, /requestには追加しないでください/);
  assert.doesNotMatch(notes, /``schemaVersion``を10へ更新/);
  assert.match(notes, /``PROTOCOL_SCHEMA_VERSION = 10``/);
  assert.match(notes, /保存済みの結果やcache/);
  assert.match(notes, new RegExp(textlintContract.compatibility.nodeEngine.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  assert.match(notes, new RegExp(textlintContract.compatibility.textlintVersion.replaceAll(".", "\\.")));
  assert.doesNotMatch(notes, /``npx --package``/);
  assert.doesNotMatch(notes, /再設計後のProcessor/);
  assert.match(notes, /日本語、中国語、韓国語の文書をHTMLへ変換している場合/);
  assert.match(notes, /textlint Processorの公開API、TxtASTへの変換結果および自動修正を行わない保証は変更していません/);
  assert.match(notes, /バージョンの異なる配布物を混ぜて使えない/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /gh attestation verify/);
  assert.match(
    notes,
    new RegExp(`\`--version --json\`が\`${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}\`を返す`),
  );
  assert.match(notes, /以前のVSIXとnative directoryを保持/);
  assert.match(notes, /rollback時は旧directoryをdev extensionとして選び直し、Zedを再起動/);
  assert.match(notes, /以前の検証済みURLへ戻し、lockfileから依存を再導入/);
  assert.match(notes, /すべてのZedプロセスを終了してから、エラーに表示されたロックのpathを削除/);
  assert.match(notes, /registryへpackageまたは拡張を公開しません/);
  assert.match(notes, /Developer ID署名とnotarizationを行わず/);
  assert.match(notes, /Authenticode署名を行いません/);
  assert.match(notes, /引用表示の組み立ては利用側アプリの責務です/);
  assert.match(notes, /解決結果を渡さない引用の表示は`unresolved_references`の設定に従い/);
  assert.match(notes, /複数ファイルの解析にはディレクトリのworkspace folderが必要/);
  assert.match(notes, /要求へ応答するthreadの外で行います/);
  assert.match(notes, /初期走査だけに適用/);
  assert.match(
    notes,
    new RegExp(`WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}`),
  );
  assert.match(notes, new RegExp(`Worker protocol versionは${protocol.workerProtocolVersion}`));
  assert.match(notes, new RegExp(`統一package version：${RELEASE_NOTES_VERSION}`));
  assert.match(notes, new RegExp(`release manifest schema version：${manifest.schemaVersion}`));
  assert.match(notes, new RegExp(`対応Rust toolchain：${manifest.rustVersion}`));

  const requestFields = protocol.request.fields.map((field) => field.json);
  assert.equal(requestFields.includes("schemaVersion"), false);
  assert.equal(requestFields.includes("packageVersion"), true);
  assert.equal(protocol.request.unknownFields, "reject");
  assert.equal(protocol.definitions.RenderInputs.fields.find(
    (field) => field.json === "generatedBibliography",
  )?.default, null);
});

test("破壊的変更が無いreleaseでは定型文を記録から生成する", () => {
  assert.deepEqual(breakingContractNotes([]), ["Rust APIの破壊的変更：ありません。"]);
  assert.deepEqual(breakingMigrationNotes([]), []);
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
  const expectedError = new RegExp(`v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}専用`);
  if (PREVIOUS_RELEASE_VERSION !== RELEASE_NOTES_VERSION) {
    assert.throws(() => buildReleaseNotes(`v${PREVIOUS_RELEASE_VERSION}`), expectedError);
  }
  assert.throws(() => buildReleaseNotes("v9.9.9"), expectedError);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
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
  assert.equal(PREVIOUS_RELEASE_VERSION, "0.30.0");
  assert.match(notes, /一つの機械可読な契約へ集約/);
  assert.match(notes, /公開用``package\.json``と収録ディレクトリを契約とrelease versionから生成/);
  assert.match(notes, /生成処理と実装を共有しないarchive検証器/);
  assert.match(notes, /固定したconsumer依存とinstall後のfile tree/);
  assert.match(notes, /別々の構築環境から同じbyte列のtarball/);
  assert.match(notes, /公開前のcandidateと公開後のGitHub Release assetを``npx``で実行/);
  assert.match(notes, /単体検査、契約検査、consumer検査、再現性検査および文書校正のtaskを分離/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /WASM protocol schema version/);
  assert.match(notes, /v0\.23\.0から変更していません/);
  assert.match(notes, /schema versionは4のままで、項目を追加も削除もしていません/);
  assert.match(notes, /破壊的変更：ありません/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /CLI、パーサ、HTMLコンパイラ、Language Server、Browser packageおよびtextlint Processorの利用方法に変更はなく/);
  assert.match(notes, new RegExp(textlintContract.identity.packageName.replace("/", "\\/")));
  assert.match(notes, new RegExp(textlintContract.identity.pluginName.replace("/", "\\/")));
  assert.match(notes, new RegExp(textlintContract.compatibility.nodeEngine.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  assert.match(notes, new RegExp(textlintContract.compatibility.textlintVersion.replaceAll(".", "\\.")));
  assert.match(notes, /``npx --package``/);
  assert.match(notes, /日本語規則、用語集および対象文書一覧は公開パッケージへ含めません/);
  assert.match(notes, /WASM protocolのschema version、Worker protocol versionおよびfield構造は変えず/);
  assert.match(notes, /``parseText``は専用の``adocweave-textlint-wasm``だけに含み/);
  assert.match(notes, /Browser packageには含めません/);
  assert.match(notes, /パーサAPI、HTMLコンパイラ、Language ServerおよびBrowser APIの動作は変更していません/);
  assert.match(notes, /textlint Processorの公開API、TxtASTへの変換結果および自動修正を行わない保証は変更していません/);
  assert.match(notes, new RegExp(`\`\`packageVersion\`\`だけを${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}へ更新`));
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
  assert.match(notes, /以前の検証済みURLへ戻し、lockfileから依存を再導入/);
  assert.match(notes, /すべてのZedプロセスを終了してから、エラーに表示されたロックのpathを削除/);
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
  const expectedError = new RegExp(`v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}専用`);
  if (PREVIOUS_RELEASE_VERSION !== RELEASE_NOTES_VERSION) {
    assert.throws(() => buildReleaseNotes(`v${PREVIOUS_RELEASE_VERSION}`), expectedError);
  }
  assert.throws(() => buildReleaseNotes("v9.9.9"), expectedError);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

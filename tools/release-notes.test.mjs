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

test("Release Notesは日本語の受入契約を常に含む", () => {
  const notes = buildReleaseNotes(`v${RELEASE_NOTES_VERSION}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /## 主な変更/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /#85：`adocweave preview`/);
  assert.match(notes, /起点文書/);
  assert.match(notes, /まだ存在しないinclude対象/);
  assert.match(notes, /最も新しい更新だけ/);
  assert.match(notes, /127\.0\.0\.1:4000/);
  assert.match(notes, /--allow-external/);
  assert.match(notes, /schema 5から6へ破壊的に更新/);
  assert.match(notes, /bindings、型定義およびfixtureをschema 6から再生成/);
  assert.match(notes, /入れ子のobject（nested object）/);
  assert.match(notes, /tagged union/);
  assert.match(notes, /WASM adapterはcoreが生成したprojection JSON/);
  assert.match(notes, /内部契約にないfieldを検出すると失敗/);
  assert.match(notes, /保存済みの出力をschema versionで区別/);
  assert.match(notes, /Rust APIの破壊的変更です/);
  assert.match(notes, /`LocalResourcePolicy::new\(\.\.\.\)`/);
  assert.match(notes, /`LocalFilesystemPolicy::new\(\.\.\.\)`/);
  assert.match(notes, /`policy\.session\(\)`/);
  assert.match(notes, /`session\.read_utf8\(LogicalSourceId::new/);
  assert.match(notes, /`LoadedFilesystemSource`/);
  assert.match(notes, /`session\.read_target_utf8\(LogicalSourceId::new/);
  assert.match(notes, /解決専用の公開APIは廃止/);
  assert.match(notes, /解決、読込、UTF-8検証および予算計上を一体で実行/);
  assert.match(notes, /sessionが所有する共有budget/);
  assert.match(notes, /byte数の差分を反映/);
  assert.match(notes, /上限超過時は以前の計上値を維持/);
  assert.match(notes, /`session\.release\(path\)`/);
  assert.match(notes, /`scan_filesystem_with_session\(&mut session\)`/);
  assert.match(notes, /Browser APIとWASM responseの移行/);
  assert.match(notes, /`AdocWeaveResult\.result`/);
  assert.match(notes, /`result\.result\.projection`/);
  assert.match(notes, /`result\.projection`/);
  assert.match(notes, /`notices: ProjectedReferenceNotice\[\]`/);
  assert.match(notes, /`reference-resolution-fallback`/);
  assert.match(notes, /`AdocWeaveWorkerClient`も維持/);
  assert.match(notes, /callback内の例外/);
  assert.match(notes, /`projection`は`null`/);
  assert.match(notes, /ファイルの情報を定期的に確認/);
  assert.match(notes, /定期的なハッシュ値確認/);
  assert.match(notes, /停止通知に対応しない処理段階は完了まで待ち/);
  assert.match(notes, /新しい接続を応答せずに閉じ/);
  assert.match(notes, /`PermissionDenied`/);
  assert.match(notes, /別の許可root内にある場合も/);
  assert.match(notes, /`OutsideRoots`として拒否/);
  assert.match(notes, /`HandleRelative`方式/);
  assert.match(notes, /macOSとWindowsは`StaticSnapshotOnly`方式/);
  assert.match(notes, /workspaceを同時変更しない、信頼済みのworkspace/);
  assert.match(notes, /WASM結果の`packageVersion`がBrowser packageと一致しない場合.*解析Promiseを`unsupported-package-version`でreject/s);
  assert.match(notes, /Worker応答の`version`が解析要求の`version`と一致しない場合.*解析Promiseを`invalid-worker-response`でreject/s);
  assert.match(notes, /staleな応答は`onResult`にも`onError`にも通知しません/);
  assert.match(notes, /terminal errorでは未完了の解析Promiseをreject/);
  assert.match(notes, /`onError`をmicrotaskで通知/);
  assert.match(notes, /同期的な実行順には依存しない/);
  assert.doesNotMatch(notes, /Rust APIに破壊的変更はありません/);
  assert.doesNotMatch(notes, /移行するための設定変更は不要です/);
  assert.match(notes, /任意のファイルやディレクトリ一覧を配信せず/);
  assert.match(notes, /公式Playgroundはこのリリースに含みません/);
  assert.match(notes, /利用者認証とTLS/);
  assert.match(notes, /registryへpackageまたは拡張を公開しません/);
  assert.match(notes, /Developer ID署名とnotarizationを行わず/);
  assert.match(notes, /Authenticode署名を行いません/);
  assert.match(notes, /以前のVSIXとnative directoryを保持/);
  assert.match(
    notes,
    new RegExp(`WASM protocol schema version：${RELEASE_NOTES_PROTOCOL_SCHEMA_VERSION}`),
  );
  assert.match(notes, new RegExp(`Worker protocol version：${protocol.workerProtocolVersion}`));
  assert.match(notes, new RegExp(`統一package version：${RELEASE_NOTES_VERSION}`));
  assert.match(notes, new RegExp(`release manifest schema version：${manifest.schemaVersion}`));
  assert.match(notes, new RegExp(`distribution plan schema version：2`));
  assert.match(notes, /配布manifest schema version：2/);
  assert.match(notes, new RegExp(`対応Rust toolchain：${manifest.rustVersion}`));
});

test("Release Notesは別release trainのtagを拒否する", () => {
  if (manifest.packageVersion !== RELEASE_NOTES_VERSION) {
    assert.throws(() => buildReleaseNotes(`v${manifest.packageVersion}`), /v0\.17\.0専用/);
  }
  assert.throws(() => buildReleaseNotes("v9.9.9"), /v0\.17\.0専用/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

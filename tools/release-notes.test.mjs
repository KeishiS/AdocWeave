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
  assert.match(notes, /公開Rust APIとWASM protocol schemaに互換性へ影響する変更/);
  assert.match(notes, /wire型を公開schemaから生成/);
  assert.match(notes, /filesystemの列挙と読込をhostへ集約/);
  assert.match(notes, /resource上限を、filesystem読込、Workspaceが保持するdisk・overlay/);
  assert.match(notes, /文書外属性と属性展開上限を一つの検証済み処理契約へ統合/);
  assert.match(notes, /前処理、解析、Lintおよび生成元への位置投影へ協調キャンセルを伝播/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /前処理設定へdefault付きの属性展開上限を追加/);
  assert.match(notes, /worker envelopeは変更していません/);
  assert.match(notes, /CLI引数およびHTML契約.*v0\.18\.0から変更していません/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /`WasmPreprocessResponse::package_version`は`&'static str`から所有値の`String`/);
  assert.match(notes, /response\.package_version\.as_str\(\)/);
  assert.match(notes, /VERSION\.to_owned\(\)/);
  assert.match(notes, /`WasmSourceMapSegment::mapping`は`String`から`WasmSourceMapping`/);
  assert.match(notes, /WasmSourceMapping::Identity/);
  assert.match(notes, /WasmSourceMapping::WholeOrigin/);
  assert.match(notes, /`PreprocessOptions`と`WasmPreprocessOptions`へ`max_attribute_expansion_depth`/);
  assert.match(notes, /WasmPreprocessOptions::default/);
  assert.match(notes, /EffectiveProcessingOptions::new/);
  assert.match(notes, /PreprocessedAnalysisError::Options/);
  assert.match(notes, /WorkspaceErrorCode::InvalidOptions/);
  assert.match(notes, /`scan_filesystem`、`scan_filesystem_with_session`、`FilesystemResource`/);
  assert.match(notes, /LocalFilesystemSession::scan_utf8/);
  assert.match(notes, /hostの`ResourceError`/);
  assert.match(notes, /`adocweave_host::DependencyGraph`の公開を終了/);
  assert.match(notes, /WorkspaceAnalysis::dependencies/);
  assert.match(notes, /`adocweave_host::ResourceLimits`はfilesystem読込専用の`FilesystemReadLimits`/);
  assert.match(notes, /`adocweave_workspace::ResourceLimits`はdisk・overlay保持専用/);
  assert.match(notes, /`ResourceSettings::limits`は`ResourceSettings::limit_plan`/);
  assert.match(notes, /`ResolvedResourceLimitPlan::filesystem_reads`/);
  assert.match(notes, /`retained_layers`、解析対象の選択は`analysis_snapshot`/);
  assert.match(notes, /`.adocweave.toml`の`resources.max-files`/);
  assert.match(notes, /同じresourceのdiskとoverlayを別々に加算/);
  assert.match(notes, /一つのrootから参照できる有効resourceだけを数えます/);
  assert.match(notes, /WASM protocol schema 7/);
  assert.match(notes, /`maxAttributeExpansionDepth`と`maxAttributeExpansionBytes`/);
  assert.match(notes, /省略時は従来と同じ32と1048576/);
  assert.match(notes, /不一致は処理前に`invalid-options`/);
  assert.match(notes, /`PreprocessedAnalysisError`へ`Cancelled`を追加/);
  assert.match(notes, /網羅的に`match`するRustコード/);
  assert.match(notes, /preprocess_and_analyze_cancellable_with_options/);
  assert.match(notes, /PreprocessedAnalysis::project_origins_cancellable/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /gh attestation verify/);
  assert.match(
    notes,
    new RegExp(`\`--version --json\`が\`${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}\`を返す`),
  );
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
  assert.throws(() => buildReleaseNotes("v0.18.0"), /v0\.19\.0専用/);
  assert.throws(() => buildReleaseNotes("v9.9.9"), /v0\.19\.0専用/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

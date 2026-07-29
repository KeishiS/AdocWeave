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
  assert.match(notes, /公開Rust APIに互換性のない型変更/);
  assert.match(notes, /wire型を公開schemaから生成/);
  assert.match(notes, /CLI、Language Server、構文解析、前処理、HTML生成および診断の責務を分割/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /macOS 14\.0以降/);
  assert.match(notes, /Windows 10 version 1809（build 10\.0\.17763）以降/);
  assert.match(notes, /v0\.18\.0からJSONのfield名、列挙値およびworker envelopeを変更していません/);
  assert.match(notes, /CLI引数、診断code、HTML契約.*v0\.18\.0から変更していません/);
  assert.match(notes, new RegExp(`## v${RELEASE_NOTES_VERSION.replaceAll(".", "\\.")}への移行`));
  assert.match(notes, /`WasmPreprocessResponse::package_version`は`&'static str`から所有値の`String`/);
  assert.match(notes, /response\.package_version\.as_str\(\)/);
  assert.match(notes, /VERSION\.to_owned\(\)/);
  assert.match(notes, /`WasmSourceMapSegment::mapping`は`String`から`WasmSourceMapping`/);
  assert.match(notes, /WasmSourceMapping::Identity/);
  assert.match(notes, /WasmSourceMapping::WholeOrigin/);
  assert.match(notes, /browser、WorkerまたはJSON APIの利用コードに移行は不要/);
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

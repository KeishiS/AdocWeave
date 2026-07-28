import assert from "node:assert/strict";
import test from "node:test";

import { buildReleaseNotes, validateReleaseNotes } from "./release-notes.mjs";
import manifest from "../release-manifest.json" with { type: "json" };
import protocol from "../protocol/public-api.json" with { type: "json" };

test("Release Notesは日本語の受入契約を常に含む", () => {
  const notes = buildReleaseNotes(`v${manifest.packageVersion}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /## 主な変更/);
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /aarch64-apple-darwin/);
  assert.match(notes, /x86_64-pc-windows-msvc/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /VS Code拡張を追加/);
  assert.match(notes, /native archive形式を全platformでflatなZIPへ統一/);
  assert.match(notes, /未信頼workspace/);
  assert.match(notes, /未対応platformではmanaged downloadを開始しません/);
  assert.match(notes, /VSIXは決定的にbuild/);
  assert.match(notes, /registryへpackageまたは拡張を公開しません/);
  assert.match(notes, /Developer ID署名とnotarizationを行わず/);
  assert.match(notes, /Authenticode署名を行いません/);
  assert.match(notes, /以前のVSIXとnative directoryを保持/);
  assert.match(notes, new RegExp(`WASM protocol schema version：${protocol.schemaVersion}`));
  assert.match(notes, new RegExp(`Worker protocol version：${protocol.workerProtocolVersion}`));
  assert.match(notes, /古いrequestとWorker envelopeは拒否/);
  assert.match(notes, new RegExp(`統一package version：${manifest.packageVersion}`));
  assert.match(notes, new RegExp(`release manifest schemaをversion 2から${manifest.schemaVersion}`));
  assert.match(notes, /distribution plan schemaをversion 1から2/);
  assert.match(notes, /配布manifest schemaをversion 1から2/);
  assert.match(notes, new RegExp(`対応Rust toolchain：${manifest.rustVersion}`));
});

test("Release Notesは別release trainのtagを拒否する", () => {
  assert.throws(() => buildReleaseNotes("v9.9.9"), /一致しません/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

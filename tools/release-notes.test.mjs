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
  assert.match(notes, /ソースブロックのタイトル/);
  assert.match(notes, /インライン数式とブロック数式/);
  assert.match(notes, /公開適合性fixture/);
  assert.match(notes, /rendering_features/);
  assert.match(notes, /Promise形式の`analyze\(\)`/);
  assert.match(notes, /WASM protocolはschema 4から5へ更新/);
  assert.match(notes, /SourceBlockProjection/);
  assert.match(notes, /figure\.source-block/);
  assert.match(notes, /data-math-language/);
  assert.match(notes, /defaultAssetUrls/);
  assert.match(notes, /stableContract/);
  assert.match(notes, /raw HTML/);
  assert.match(notes, /数式engine/);
  for (const issue of [108, 109, 111, 110, 81]) {
    assert.match(notes, new RegExp(`#${issue}：`));
  }
  assert.match(notes, /registryへpackageまたは拡張を公開しません/);
  assert.match(notes, /Developer ID署名とnotarizationを行わず/);
  assert.match(notes, /Authenticode署名を行いません/);
  assert.match(notes, /以前のVSIXとnative directoryを保持/);
  assert.match(notes, new RegExp(`WASM protocol schema version：${protocol.schemaVersion}`));
  assert.match(notes, new RegExp(`Worker protocol version：${protocol.workerProtocolVersion}`));
  assert.match(notes, new RegExp(`統一package version：${manifest.packageVersion}`));
  assert.match(notes, new RegExp(`release manifest schema version：${manifest.schemaVersion}`));
  assert.match(notes, new RegExp(`distribution plan schema version：2`));
  assert.match(notes, /配布manifest schema version：2/);
  assert.match(notes, new RegExp(`対応Rust toolchain：${manifest.rustVersion}`));
});

test("Release Notesは別release trainのtagを拒否する", () => {
  assert.throws(() => buildReleaseNotes("v9.9.9"), /一致しません/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

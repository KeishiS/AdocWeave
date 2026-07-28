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
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /schema version付きの`\.adocweave\.toml`/);
  assert.match(notes, /CI用の失敗閾値/);
  assert.match(notes, /`adocweave-workspace` crate/);
  assert.match(notes, /複数file、directoryおよびglob/);
  assert.match(notes, /プロジェクト設定schema version 1/);
  assert.match(notes, /既定値は`--fail-on error`/);
  assert.match(notes, /GitHub ActionsおよびSARIF/);
  assert.match(notes, /`Applicability::Always`/);
  assert.match(notes, /型付きresource ID、revisionおよびgeneration/);
  assert.match(notes, /`ResourceDocument\.source`を`String`から`Arc<str>`へ変更/);
  assert.match(notes, /crates\.ioへは公開しません/);
  assert.match(notes, /相対linkはhostが有効なURL policy/);
  assert.match(notes, /相対URLを明示的に許可/);
  assert.match(notes, /filesystemが変化しないことを仮定/);
  assert.match(notes, /Issue #56/);
  assert.match(notes, /adocweave check --fail-on warning --format json --summary docs/);
  assert.match(notes, /schema-version = 1/);
  assert.match(notes, /所有するtextを`Arc<str>`へ変換/);
  assert.match(notes, new RegExp(`WASM protocol schema version：${protocol.schemaVersion}`));
  assert.match(notes, new RegExp(`Worker protocol version：${protocol.workerProtocolVersion}`));
  assert.match(notes, /古いrequestとWorker envelopeは拒否/);
  assert.match(notes, /信頼済みDOMにするものではありません/);
  assert.match(notes, new RegExp(`統一package version：${manifest.packageVersion}`));
  assert.match(notes, new RegExp(`対応Rust toolchain：${manifest.rustVersion}`));
});

test("Release Notesは別release trainのtagを拒否する", () => {
  assert.throws(() => buildReleaseNotes("v9.9.9"), /一致しません/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /必須見出し/);
});

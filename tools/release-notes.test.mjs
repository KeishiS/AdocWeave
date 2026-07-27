import assert from "node:assert/strict";
import test from "node:test";

import { appendRequiredReleaseNotes, validateReleaseNotes } from "./release-notes.mjs";
import manifest from "../release-manifest.json" with { type: "json" };
import protocol from "../protocol/public-api.json" with { type: "json" };

test("release notes always contain the acceptance contract", () => {
  const notes = appendRequiredReleaseNotes("Generated changes", `v${manifest.packageVersion}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /set, redefined, and unset in source order/);
  assert.match(notes, /Multiline soft and hard attribute-value continuations/);
  assert.match(notes, /Include targets and conditional directives use the attribute state/);
  assert.match(notes, /effective values, selected binding IDs/);
  assert.match(notes, /`ResolvedAttribute\.binding` is optional/);
  assert.match(notes, /hard-locked set values and hard-locked unset values/);
  assert.match(notes, /`duplicate-attribute` lint rule is removed/);
  assert.match(notes, /Hover, Definition, References, and Completion/);
  assert.match(notes, /By default, a relative link remains inactive in HTML until a host resolves it/);
  assert.match(notes, /Hosts may explicitly allow authored relative URLs/);
  assert.match(notes, /filesystem does not change during one command/);
  assert.match(notes, /tracked in issue #56/);
  assert.match(notes, /attribute_environment\(\)\.resolve_at/);
  assert.match(notes, /analysisOptions: \{ attributes/);
  assert.match(notes, new RegExp(`WASM protocol schema version: ${protocol.schemaVersion}`));
  assert.match(notes, new RegExp(`Worker protocol version: ${protocol.workerProtocolVersion}`));
  assert.match(notes, /Older requests and Worker envelopes are rejected/);
  assert.match(notes, /attributeQueries: true/);
  assert.match(notes, /preprocess: \{ resources \}/);
  assert.match(notes, /does not make generated markup a trusted DOM/);
  assert.match(notes, new RegExp(`unified package version: ${manifest.packageVersion}`));
  assert.match(notes, new RegExp(`Supported Rust toolchain: ${manifest.rustVersion}`));
});

test("release notes reject a tag from another release train", () => {
  assert.throws(() => appendRequiredReleaseNotes("", "v9.9.9"), /does not match/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /missing/);
});

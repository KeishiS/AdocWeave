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
  assert.match(notes, /scheme-less relative links/);
  assert.match(notes, /keeps unresolved relative links inactive/);
  assert.match(notes, /Nu HTML Checker/);
  assert.match(notes, /Complete HTML documents.*doctype.*html lang attribute.*UTF-8 meta.*non-empty title/);
  assert.match(notes, /serialized bytes and DOM shape/);
  assert.match(notes, /relative link is activated in HTML only after a host resolves it/);
  assert.match(notes, new RegExp(`WASM protocol schema version: ${protocol.schemaVersion}`));
  assert.match(notes, /does not change the public Rust API or protocol schema/);
  assert.match(notes, /does not make generated markup a trusted DOM/);
  assert.match(notes, new RegExp(`unified package version: ${manifest.packageVersion}`));
  assert.match(notes, new RegExp(`Supported Rust toolchain: ${manifest.rustVersion}`));
});

test("release notes reject a tag from another release train", () => {
  assert.throws(() => appendRequiredReleaseNotes("", "v9.9.9"), /does not match/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /missing/);
});

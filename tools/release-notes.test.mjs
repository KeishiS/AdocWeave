import assert from "node:assert/strict";
import test from "node:test";

import { appendRequiredReleaseNotes, validateReleaseNotes } from "./release-notes.mjs";
import manifest from "../release-manifest.json" with { type: "json" };

test("release notes always contain the acceptance contract", () => {
  const notes = appendRequiredReleaseNotes("Generated changes", `v${manifest.packageVersion}`);
  assert.doesNotThrow(() => validateReleaseNotes(notes));
  assert.match(notes, /x86_64-unknown-linux-musl/);
  assert.match(notes, /sha256sum --check/);
  assert.match(notes, /approved AsciiDoc compatibility changes/);
  assert.match(notes, /nix run github:KeishiS\/AdocWeave/);
  assert.match(notes, /unified public contract: 2/);
});

test("release notes reject a tag from another release train", () => {
  assert.throws(() => appendRequiredReleaseNotes("", "v9.9.9"), /does not match/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /missing/);
});

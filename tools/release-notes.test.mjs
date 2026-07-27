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
  assert.match(notes, /validate local xref, link, media, and include targets/);
  assert.match(notes, /`asciidoc-file-link` and `non-asciidoc-xref`/);
  assert.match(notes, /malformed percent encoding, encoded controls, and network-path references/);
  assert.match(notes, /normal relative document targets remain valid authored input/);
  assert.match(notes, /opt-in `macro-boundary` rule/);
  assert.match(notes, /existing default diagnostics unchanged/);
  assert.match(notes, /check --local-targets --project-root <DIR>/);
  assert.match(notes, /missing targets, root escapes, and symlink escapes/);
  assert.match(notes, /repeatable `--enable-rule`/);
  assert.match(notes, /`enabledByDefault: false`/);
  assert.match(notes, /adocweave\.enabledRules/);
  assert.match(notes, /reanalyzes open documents/);
  assert.match(notes, /Authored URL validation.*active rendered URL validation.*local filesystem target inspection/);
  assert.match(notes, /By default, a relative link remains inactive in HTML until a host resolves it/);
  assert.match(notes, /Hosts may explicitly allow authored relative URLs/);
  assert.match(notes, /filesystem does not change during one command/);
  assert.match(notes, /tracked in issue #56/);
  assert.match(notes, /AnalysisOptions\.diagnostics\.lint\.authored_url_policy/);
  assert.match(notes, /allow_authored_relative: true/);
  assert.match(notes, /adocweave::output::html::RenderPolicy/);
  assert.match(notes, /adocweave::resolution::ActiveUrlPolicy/);
  assert.match(notes, new RegExp(`WASM protocol schema version: ${protocol.schemaVersion}`));
  assert.match(notes, /previous flat options object is rejected/);
  assert.match(notes, /replaces ParseOptions with AnalysisOptions/);
  assert.match(notes, /Update browser requests from the removed flat `options`/);
  assert.match(notes, /Engine::new\(adocweave::AnalysisOptions/);
  assert.match(notes, /analysisOptions:.*syntaxMode/s);
  assert.match(notes, /renderPolicy:.*activeUrls/s);
  assert.match(notes, /outputLimits:.*maxOutputBytes/s);
  assert.match(notes, /does not make generated markup a trusted DOM/);
  assert.match(notes, new RegExp(`unified package version: ${manifest.packageVersion}`));
  assert.match(notes, new RegExp(`Supported Rust toolchain: ${manifest.rustVersion}`));
});

test("release notes reject a tag from another release train", () => {
  assert.throws(() => appendRequiredReleaseNotes("", "v9.9.9"), /does not match/);
  assert.throws(() => validateReleaseNotes("Generated changes"), /missing/);
});

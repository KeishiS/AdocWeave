import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));
const protocol = JSON.parse(readFileSync(new URL("protocol/public-api.json", ROOT), "utf8"));

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## Supported targets",
  "## Public contracts and breaking changes",
  "## Known constraints",
  "## Asset verification",
  "## Upgrade and rollback",
];

const highlights = [
  "Public analysis, diagnostics, active URL rendering, and output limits now use responsibility-specific configuration types.",
  "The CLI can emit the complete typed lint rule catalog, including activation capability, with `adocweave check --list-rules --json`.",
  "The CLI can validate local xref, link, media, and include targets below an explicit project root without moving filesystem access into the core analyzer.",
  "Default lint now reports `asciidoc-file-link` and `non-asciidoc-xref` when link and xref macro choice disagrees with the target type, and offers fixes only when the rewrite is unambiguous.",
  "Authored and active URL policies now reject malformed percent encoding, encoded controls, and network-path references while normal relative document targets remain valid authored input.",
  "The opt-in `macro-boundary` rule detects complete inline macros that fail only their start-boundary requirement while leaving existing default diagnostics unchanged.",
];

const contractNotes = [
  `unified package version: ${manifest.packageVersion}`,
  `WASM protocol schema version: ${protocol.schemaVersion}. The request now separates analysisOptions, renderPolicy, and outputLimits; the previous flat options object is rejected.`,
  "The Rust API replaces ParseOptions with AnalysisOptions, SyntaxOptions, and DiagnosticProfile; ProcessingLimits becomes AnalysisLimits plus OutputLimits.",
  "LintRule is replaced by stable LintRuleId values and the LINT_RULES descriptor catalog.",
  "UrlPolicy and UrlContext are replaced by AuthoredUrlPolicy, ActiveUrlPolicy, and UrlProvenance.",
  "Authored URL validation, active rendered URL validation, and local filesystem target inspection are separate policies owned by the analyzer, renderer, and host adapter respectively.",
  "Local target validation is opt-in through `adocweave check --local-targets --project-root <DIR>` and rejects missing targets, root escapes, and symlink escapes.",
  "The `macro-boundary` rule is opt-in through repeatable `--enable-rule` options and is published with `enabledByDefault: false`.",
  "Language Server clients can enable opt-in diagnostics through `adocweave.enabledRules`; changing the setting reanalyzes open documents.",
];

const knownConstraints = [
  `Supported Rust toolchain: ${manifest.rustVersion}, fixed by this release's flake.lock.`,
  "Native binaries are available only for Linux x86-64 and ARM64.",
  "By default, a relative link remains inactive in HTML until a host resolves it to a URL accepted by the active URL policy. Hosts may explicitly allow authored relative URLs.",
  "Local target validation assumes that the filesystem does not change during one command. Hardening against concurrent symlink replacement is tracked in issue #56.",
  "HTML5 validation checks standards conformance; it does not make generated markup a trusted DOM.",
  "The Zed extension is installed as a development extension; it is not published to the Zed Extension Gallery.",
  "Packages are not published to crates.io, npm, or OS package registries. The Nix package is built directly from this repository flake.",
];

function markdownList(items) {
  return items.map((item) => `- ${item}`).join("\n");
}

export function appendRequiredReleaseNotes(body, tag) {
  if (tag !== `v${manifest.packageVersion}`) throw new Error("release note tag does not match package version");
  const targets = plan.targets.map((target) => `- Linux ${target}`).join("\n");
  const notes = `## Highlights\n\n${markdownList(highlights)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${markdownList(contractNotes)}\n\n` +
    "This release requires consumers to match the listed package version exactly. Do not mix CLI, LSP, browser, or Zed assets from different versions. Hosts must request `resourceQueries`, resolve every successful resource with a concrete MIME type, and rebuild `RenderInputs` after each document revision.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n${markdownList(knownConstraints)}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n` +
    "Download all release assets, run `sha256sum --check sha256.sum`, then verify required assets with `gh attestation verify <asset> --repo KeishiS/AdocWeave`.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "Update Rust integrations to configure authored URLs through `AnalysisOptions.diagnostics.lint.authored_url_policy` and active output through `RenderPolicy.active_urls`.\n\n" +
    "```rust\nlet mut lint = adocweave::output::diagnostics::LintConfig::default();\n" +
    "lint.authored_url_policy.allow_relative = true;\n" +
    "let engine = adocweave::Engine::new(adocweave::AnalysisOptions {\n" +
    "    diagnostics: adocweave::DiagnosticProfile { lint },\n" +
    "    ..adocweave::AnalysisOptions::default()\n" +
    "});\n" +
    "let policy = adocweave::output::html::RenderPolicy {\n" +
    "    active_urls: adocweave::resolution::ActiveUrlPolicy {\n" +
    "        allow_authored_relative: true,\n" +
    "        ..adocweave::resolution::ActiveUrlPolicy::default()\n" +
    "    },\n" +
    "    ..adocweave::output::html::RenderPolicy::default()\n" +
    "};\n```\n\n" +
    "Update browser requests from the removed flat `options` object as follows.\n\n" +
    "```js\nclient.update({\n  version: 1,\n  source,\n  analysisOptions: { syntax: { syntaxMode: \"strict\" } },\n  renderPolicy: { activeUrls: { allowResolvedRelative: true } },\n  outputLimits: { maxOutputBytes: 1048576 },\n});\n```\n\n" +
    "Install into a versioned directory and switch the `current` symlink only after verification. Keep the previous version until acceptance succeeds; rollback by restoring that symlink. See `docs/user-guide/release-installation.adoc`.\n";
  return `${body.trim()}\n\n${notes}`;
}

export function validateReleaseNotes(body) {
  for (const heading of REQUIRED_RELEASE_NOTE_HEADINGS) {
    if (!body.includes(heading)) throw new Error(`release notes are missing: ${heading}`);
  }
}

if (process.argv[1] && import.meta.url === new URL(process.argv[1], "file:").href) {
  const tag = process.argv[2];
  let input = "";
  for await (const chunk of process.stdin) input += chunk;
  const output = appendRequiredReleaseNotes(input, tag);
  validateReleaseNotes(output);
  process.stdout.write(output);
}

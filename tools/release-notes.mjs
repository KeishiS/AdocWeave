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
  "Document attributes can now be set, redefined, and unset in source order; later definitions do not affect earlier references.",
  "Multiline soft and hard attribute-value continuations preserve their source ranges while exposing a folded evaluation value.",
  "Include targets and conditional directives use the attribute state visible when each directive is read, including changes made by included documents.",
  "Lint, Language Server features, native analysis, and WASM now share the same positioned attribute bindings and references.",
  "WASM attribute queries include effective values, selected binding IDs, and original source IDs and ranges for included content.",
];

const contractNotes = [
  `unified package version: ${manifest.packageVersion}`,
  `WASM protocol schema version: ${protocol.schemaVersion}; Worker protocol version: ${protocol.workerProtocolVersion}. Older requests and Worker envelopes are rejected.`,
  "WASM requests can select `attributeQueries` and optionally provide a preprocessing resource snapshot. Responses return typed bindings, references, effective values, errors, and source provenance.",
  "The Rust API exposes `AttributeEnvironment`, `AttributeQueryProduct`, binding histories, final values, and position-dependent resolution.",
  "`ResolvedAttribute.binding` is optional because a hard-locked external attribute has no authored source occurrence.",
  "`AnalysisOptions.attributes` accepts hard-locked set values and hard-locked unset values. Authored operations cannot override them.",
  "The `duplicate-attribute` lint rule is removed. Valid redefinitions are not diagnostics; undefined, unused, cycle, depth, and size checks operate on positioned bindings.",
  "Attribute Hover, Definition, References, and Completion use the binding visible at the cursor and project included definitions to their original documents.",
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
    "Update Rust integrations to resolve attributes at the consumer's source position. Treat a missing binding as an external value rather than a missing attribute.\n\n" +
    "```rust\nlet resolved = analysis.attribute_environment().resolve_at(\"name\", offset);\n" +
    "if let Some(resolved) = resolved {\n" +
    "    let value = resolved.value?;\n" +
    "    let authored_definition = resolved.binding;\n" +
    "}\n```\n\n" +
    "Browser integrations must request the new product explicitly and provide already-fetched include resources when preprocessing is required.\n\n" +
    "```js\nclient.update({\n  version: 1,\n  source,\n  products: { ...products, attributeQueries: true },\n  preprocess: { resources },\n  analysisOptions: { attributes: { locked: \"value\", absent: null } },\n});\n```\n\n" +
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

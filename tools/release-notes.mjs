import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("release-manifest.json", ROOT), "utf8"));
const plan = JSON.parse(readFileSync(new URL("release/distribution-plan.json", ROOT), "utf8"));

export const REQUIRED_RELEASE_NOTE_HEADINGS = [
  "## Supported targets",
  "## Public contracts and breaking changes",
  "## Known constraints",
  "## Asset verification",
  "## Upgrade and rollback",
];

export function appendRequiredReleaseNotes(body, tag) {
  if (tag !== `v${manifest.packageVersion}`) throw new Error("release note tag does not match package version");
  const contracts = `- unified package version: ${manifest.packageVersion}\n` +
    "- WASM protocol schema version: 2. Resource queries are now a requested product and resolved resources require a concrete MIME type.\n" +
    "- The Rust resource API now uses ResourcePurpose and validated MediaType values. This is an intentional breaking change.";
  const targets = plan.targets.map((target) => `- Linux ${target}`).join("\n");
  const notes = "## Highlights\n\n" +
    "- Image, icon, audio, and video output now requires a host-resolved URL and a MIME type matching the macro. Missing, rejected, or mismatched primary resources fail closed with a typed render diagnostic and escaped fallback text.\n" +
    "- Video poster resources are exposed as independent resource queries and are revalidated as images. A failed poster is omitted without disabling a safe video.\n" +
    "- Media output applies only the documented alt, title, dimension, controls, and poster attributes. Input attributes are never passed through as arbitrary HTML.\n" +
    "- The repository flake provides AdocWeave CLI and LSP packages for Linux x86-64 and ARM64. Run `nix run github:KeishiS/AdocWeave`.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${contracts}\n\n` +
    "This release requires consumers to match the listed package version exactly. Do not mix CLI, LSP, browser, or Zed assets from different versions. Hosts must request `resourceQueries`, resolve every successful resource with a concrete MIME type, and rebuild `RenderInputs` after each document revision.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n` +
    `- Supported Rust toolchain: ${manifest.rustVersion}, fixed by this release's flake.lock.\n` +
    "- Native binaries are available only for Linux x86-64 and ARM64.\n" +
    "- The Zed extension is installed as a development extension; it is not published to the Zed Extension Gallery.\n" +
    "- Packages are not published to crates.io, npm, or OS package registries. The Nix package is built directly from this repository flake.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n` +
    "Download all release assets, run `sha256sum --check sha256.sum`, then verify required assets with `gh attestation verify <asset> --repo KeishiS/AdocWeave`.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
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

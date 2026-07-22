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
  const contracts = Object.entries(manifest.contracts)
    .map(([name, version]) => `- ${name}: ${version}`)
    .join("\n");
  const targets = plan.targets.map((target) => `- Linux ${target}`).join("\n");
  const notes = "## Highlights\n\n" +
    "- v0.1.0 is functionally identical to the accepted v0.1.0-rc.6 baseline; it contains no intentional API or feature changes.\n" +
    "- The repository flake provides AdocWeave CLI and LSP packages for Linux x86-64 and ARM64. Run `nix run github:KeishiS/AdocWeave`.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[0]}\n\n${targets}\n\n` +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[1]}\n\n${contracts}\n\n` +
    "This release requires consumers to match the listed contract versions. Do not mix CLI, LSP, browser, or Zed assets from different versions.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[2]}\n\n` +
    "- Native binaries are available only for Linux x86-64 and ARM64.\n" +
    "- The Zed extension is installed as a development extension; it is not published to the Zed Extension Gallery.\n" +
    "- Packages are not published to crates.io, npm, or OS package registries. The Nix package is built directly from this repository flake.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[3]}\n\n` +
    "Download all release assets, run `sha256sum --check sha256.sum`, then verify required assets with `gh attestation verify <asset> --repo KeishiS/AdocWeave`.\n\n" +
    `${REQUIRED_RELEASE_NOTE_HEADINGS[4]}\n\n` +
    "Install into a versioned directory and switch the `current` symlink only after verification. Keep the previous version until acceptance succeeds; rollback by restoring that symlink. See `docs/release-installation.adoc`.\n";
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

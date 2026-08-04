import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import plan from "../release/distribution-plan.json" with { type: "json" };
import { cargoTreePackageKeys } from "./generate-third-party-notices.mjs";
import { verifyMetadata, writeMetadata } from "./release-metadata.mjs";

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "adocweave-release-metadata-"));
  const artifacts = join(root, "artifacts");
  mkdirSync(artifacts);
  for (const asset of plan.assets) {
    const archiveRoot = asset.name.replace(/\.(?:tar\.xz|tgz|vsix|zip)$/, "");
    const stage = join(root, archiveRoot);
    mkdirSync(stage);
    writeFileSync(join(stage, asset.executable ?? "index.mjs"), `${asset.name}\n`);
    if (asset.archive === "vsix") {
      writeFileSync(join(stage, "[Content_Types].xml"), "<Types />\n");
    }
    if (asset.archive === "zip" || asset.archive === "vsix") {
      execFileSync("zip", ["-X", "-q", "-r", join(artifacts, asset.name), archiveRoot], { cwd: root });
    } else if (asset.archive === "tgz") {
      execFileSync("tar", ["--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner",
        "-czf", join(artifacts, asset.name), "-C", root, archiveRoot]);
    } else {
      execFileSync("tar", ["--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner",
        "-cJf", join(artifacts, asset.name), "-C", root, archiveRoot]);
    }
  }
  return { root, artifacts };
}

test("actual archives produce canonical manifest, SPDX SBOM, and unified checksums", () => {
  const { root, artifacts } = fixture();
  try {
    const commit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    writeMetadata(artifacts, commit);
    verifyMetadata(artifacts, commit);
    const manifest = JSON.parse(readFileSync(join(artifacts, "adocweave-dist-manifest.json"), "utf8"));
    const sbom = JSON.parse(readFileSync(join(artifacts, "adocweave.spdx.json"), "utf8"));
    const checksums = readFileSync(join(artifacts, "sha256.sum"), "utf8").trimEnd().split("\n");
    assert.equal(manifest.assets.length, plan.assets.length);
    assert.equal(sbom.spdxVersion, "SPDX-2.3");
    assert.equal(sbom.files.length, plan.assets.length + 1);
    assert.ok(sbom.files.some((entry) =>
      entry.fileName.endsWith("/[Content_Types].xml")));
    assert.ok(sbom.files.every((entry) => entry.copyrightText === "NOASSERTION" && entry.licenseConcluded === "NOASSERTION"));
    const archivePackages = sbom.packages.filter((entry) => entry.packageFileName);
    assert.equal(archivePackages.length, plan.assets.length);
    assert.ok(archivePackages.every((entry) => /^[0-9a-f]{40}$/.test(entry.packageVerificationCode.packageVerificationCodeValue)));
    const cargoMetadata = [
      JSON.parse(execFileSync("cargo", ["metadata", "--format-version=1", "--locked"], { encoding: "utf8" })),
      JSON.parse(execFileSync("cargo", ["metadata", "--manifest-path", "editors/zed/Cargo.toml", "--format-version=1", "--locked"], { encoding: "utf8" })),
    ];
    const expectedCargo = [...new Set(cargoMetadata.flatMap((metadata) => metadata.packages)
      .map((entry) => `pkg:cargo/${encodeURIComponent(entry.name)}@${entry.version}`))].sort();
    const actualCargo = sbom.packages.flatMap((entry) => entry.externalRefs ?? [])
      .map((reference) => reference.referenceLocator)
      .filter((reference) => reference.startsWith("pkg:cargo/"))
      .sort();
    assert.deepEqual(actualCargo, expectedCargo);
    assert.ok(sbom.packages.some((entry) => entry.externalRefs?.some((reference) =>
      reference.referenceLocator === `pkg:npm/%40adocweave/browser@${plan.packageVersion}`)));
    assert.ok(sbom.packages.some((entry) => entry.externalRefs?.some((reference) =>
      reference.referenceLocator === `pkg:npm/adocweave-vscode@${plan.packageVersion}`)));
    assert.ok(sbom.packages.some((entry) => entry.externalRefs?.some((reference) =>
      reference.referenceLocator === `pkg:npm/%40adocweave/textlint-plugin-asciidoc@${plan.packageVersion}`)));
    const textlintAsset = plan.assets.find(({ kind }) => kind === "textlint-plugin");
    const textlintArchive = sbom.packages.find((entry) => entry.name === textlintAsset.name);
    const packageById = new Map(sbom.packages.map((entry) => [entry.SPDXID, entry]));
    const textlintDependencies = sbom.relationships
      .filter((entry) =>
        entry.spdxElementId === textlintArchive.SPDXID &&
        entry.relationshipType === "DEPENDS_ON"
      )
      .map((entry) => packageById.get(entry.relatedSpdxElement))
      .flatMap((entry) => entry.externalRefs ?? [])
      .map((entry) => entry.referenceLocator)
      .sort();
    const expectedTextlintDependencies = [
      ...[...cargoTreePackageKeys(
        "adocweave-textlint-wasm",
        "wasm32-unknown-unknown",
      )].map((key) => {
        const [name, version] = key.split("\0");
        return `pkg:cargo/${encodeURIComponent(name)}@${version}`;
      }),
      `pkg:npm/%40adocweave/textlint-plugin-asciidoc@${plan.packageVersion}`,
    ].sort();
    assert.deepEqual(textlintDependencies, expectedTextlintDependencies);
    assert.ok(sbom.packages.some((entry) => entry.externalRefs?.some((reference) =>
      reference.referenceLocator === "pkg:npm/fflate@0.8.3")));
    assert.deepEqual(checksums.map((line) => line.slice(66)), [
      ...plan.assets.map((asset) => asset.name),
      "adocweave-dist-manifest.json",
      "adocweave.spdx.json",
    ].sort());
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("verification rejects modified metadata and incomplete asset sets", () => {
  const { root, artifacts } = fixture();
  try {
    const commit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    writeMetadata(artifacts, commit);
    writeFileSync(join(artifacts, "sha256.sum"), "tampered\n");
    assert.throws(() => verifyMetadata(artifacts, commit), /metadata mismatch/);
    rmSync(join(artifacts, plan.assets[0].name));
    assert.throws(() => writeMetadata(artifacts, commit), /missing release archive/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("generation rejects an empty archive before manifest validation", () => {
  const { root, artifacts } = fixture();
  try {
    const commit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    writeFileSync(join(artifacts, plan.assets[0].name), "");
    assert.throws(() => writeMetadata(artifacts, commit), /empty release archive/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("verification rejects every unplanned file regardless of its extension", () => {
  const { root, artifacts } = fixture();
  try {
    const commit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    writeMetadata(artifacts, commit);
    writeFileSync(join(artifacts, "unplanned.txt"), "must not be published\n");
    assert.throws(() => verifyMetadata(artifacts, commit), /unplanned public asset/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("metadata generation rejects a symlink in the textlint plugin tarball", () => {
  const { root, artifacts } = fixture();
  try {
    const asset = plan.assets.find(({ kind }) => kind === "textlint-plugin");
    const stage = join(root, "unsafe-plugin");
    mkdirSync(stage);
    writeFileSync(join(stage, "target"), "target\n");
    symlinkSync("target", join(stage, "link"));
    execFileSync("tar", ["-czf", join(artifacts, asset.name), "-C", root, "unsafe-plugin"]);
    const commit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    assert.throws(() => writeMetadata(artifacts, commit), /symlink or unsupported member type/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("metadata generation rejects parent traversal in the textlint plugin tarball", () => {
  const { root, artifacts } = fixture();
  try {
    const asset = plan.assets.find(({ kind }) => kind === "textlint-plugin");
    const unsafe = join(root, "unsafe.txt");
    writeFileSync(unsafe, "unsafe\n");
    execFileSync("tar", [
      "-czf",
      join(artifacts, asset.name),
      "--transform=s,^,../,",
      "-C",
      root,
      "unsafe.txt",
    ]);
    const commit = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    assert.throws(() => writeMetadata(artifacts, commit), /unsafe archive member/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

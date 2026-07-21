import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

import plan from "../release/distribution-plan.json" with { type: "json" };
import { verifyMetadata, writeMetadata } from "./release-metadata.mjs";

function fixture() {
  const root = mkdtempSync(join("target", "adocweave-release-metadata-"));
  const artifacts = join(root, "artifacts");
  mkdirSync(artifacts);
  for (const asset of plan.assets) {
    const archiveRoot = asset.name.slice(0, -".tar.xz".length);
    const stage = join(root, archiveRoot);
    mkdirSync(stage);
    writeFileSync(join(stage, asset.executable ?? "index.mjs"), `${asset.name}\n`);
    execFileSync("tar", ["--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner",
      "-cJf", join(artifacts, asset.name), "-C", root, archiveRoot]);
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
    assert.equal(sbom.files.length, plan.assets.length);
    assert.ok(sbom.files.every((entry) => entry.copyrightText === "NOASSERTION" && entry.licenseConcluded === "NOASSERTION"));
    const archivePackages = sbom.packages.filter((entry) => entry.packageFileName);
    assert.equal(archivePackages.length, plan.assets.length);
    assert.ok(archivePackages.every((entry) => /^[0-9a-f]{40}$/.test(entry.packageVerificationCode.packageVerificationCodeValue)));
    const cargoMetadata = JSON.parse(execFileSync("cargo", ["metadata", "--format-version=1", "--locked"], { encoding: "utf8" }));
    const expectedCargo = cargoMetadata.packages.map((entry) => `pkg:cargo/${encodeURIComponent(entry.name)}@${entry.version}`).sort();
    const actualCargo = sbom.packages.flatMap((entry) => entry.externalRefs ?? [])
      .map((reference) => reference.referenceLocator)
      .filter((reference) => reference.startsWith("pkg:cargo/"))
      .sort();
    assert.deepEqual(actualCargo, expectedCargo);
    assert.ok(sbom.packages.some((entry) => entry.externalRefs?.some((reference) => reference.referenceLocator === "pkg:npm/%40adocweave/browser@0.1.0")));
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

import assert from "node:assert/strict";
import test from "node:test";

import { EXPECTED_RELEASE_METADATA, canonicalJson, expectedAssets, validateDistributionManifest, validateDistPlan, versionFromTag } from "./release-contract.mjs";
import plan from "../release/distribution-plan.json" with { type: "json" };
import fixture from "../release/adocweave-dist-manifest.fixture.json" with { type: "json" };

test("stable and rc tags are exact and versioned", () => {
  assert.equal(versionFromTag("v1.2.3"), "1.2.3");
  assert.equal(versionFromTag("v1.2.3-rc.4"), "1.2.3-rc.4");
  for (const invalid of ["1.2.3", "v1.2", "release/v1.2.3", "v1.2.3-alpha.1", "v1.2.3-rc.0"]) {
    assert.throws(() => versionFromTag(invalid));
  }
});

test("initial asset matrix contains only Linux native and one browser archive", () => {
  assert.deepEqual(expectedAssets(plan.packageVersion, plan.targets), plan.assets);
  assert.deepEqual(plan.targets, ["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"]);
  assert.deepEqual(plan.releaseMetadata, EXPECTED_RELEASE_METADATA);
});

test("distribution manifest fixture satisfies the public contract", () => {
  assert.doesNotThrow(() => validateDistributionManifest(fixture, plan));
  assert.equal(canonicalJson(fixture), `${JSON.stringify(fixture, null, 2)}\n`);
});

test("manifest rejects unknown, duplicate, unsorted and invalid assets", () => {
  assert.throws(() => validateDistributionManifest({ ...fixture, unexpected: true }, plan));
  assert.throws(() => validateDistributionManifest({ ...fixture, assets: [fixture.assets[1], fixture.assets[0], ...fixture.assets.slice(2)] }, plan));
  assert.throws(() => validateDistributionManifest({ ...fixture, assets: fixture.assets.map((asset, index) => index === 0 ? { ...asset, sha256: "bad" } : asset) }, plan));
});

test("dist plan validation rejects an incomplete plan", () => {
  assert.throws(() => validateDistPlan({
    dist_version: plan.distVersion,
    announcement_tag: `v${plan.packageVersion}`,
    releases: [],
    artifacts: {},
  }, plan, `v${plan.packageVersion}`));
});

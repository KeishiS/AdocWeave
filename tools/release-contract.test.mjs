import assert from "node:assert/strict";
import test from "node:test";

import { EXPECTED_RELEASE_METADATA, SUPPORTED_PUBLIC_PROTOCOL_SCHEMA_VERSION, canonicalJson, expectedAssets, validateDistributionManifest, validateDistPlan, validatePublicClientReleaseContract, validateReleaseTrainVersions, versionFromTag } from "./release-contract.mjs";
import plan from "../release/distribution-plan.json" with { type: "json" };
import fixture from "../release/adocweave-dist-manifest.fixture.json" with { type: "json" };
import protocol from "../protocol/public-api.json" with { type: "json" };
import vscodeLock from "../editors/vscode/package-lock.json" with { type: "json" };
import vscodePackage from "../editors/vscode/package.json" with { type: "json" };
import conformance from "../crates/adocweave/conformance/cases.json" with { type: "json" };
import publicConformance from "../fixtures/public-conformance.json" with { type: "json" };

test("stable tags are exact and versioned", () => {
  assert.equal(versionFromTag("v1.2.3"), "1.2.3");
  for (const invalid of ["1.2.3", "v1.2", "release/v1.2.3", "v1.2.3-alpha.1", "v1.2.3-rc.0", "v1.2.3-rc.1"]) {
    assert.throws(() => versionFromTag(invalid));
  }
});

test("asset matrix contains every declared native target, browser, and Zed archives", () => {
  assert.deepEqual(expectedAssets(plan.packageVersion, plan.targets), plan.assets);
  assert.deepEqual(
    plan.targets.map(({ triple }) => triple),
    [
      "aarch64-unknown-linux-musl",
      "aarch64-apple-darwin",
      "x86_64-unknown-linux-musl",
      "x86_64-pc-windows-msvc",
    ],
  );
  assert.equal(plan.targets.find(({ os }) => os === "win32").archive, "zip");
  assert.ok(plan.targets.filter(({ os }) => os === "darwin").every(({ minimumOsVersion }) => minimumOsVersion === "14.0"));
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

test("public client manifests match the release train and remain private", () => {
  const version = plan.packageVersion;
  assert.doesNotThrow(() =>
    validatePublicClientReleaseContract(version, vscodePackage, vscodeLock, protocol));

  const mutations = [
    [new RegExp("VS Code package version"), { ...vscodePackage, version: "9.9.9" }, vscodeLock, protocol],
    [/must remain private/, { ...vscodePackage, private: false }, vscodeLock, protocol],
    [new RegExp("VS Code package lock version"), vscodePackage, { ...vscodeLock, version: "9.9.9" }, protocol],
    [new RegExp("VS Code package lock root"), vscodePackage, {
      ...vscodeLock,
      packages: { ...vscodeLock.packages, "": { ...vscodeLock.packages[""], version: "9.9.9" } },
    }, protocol],
    [/lockfileVersion must be 3/, vscodePackage, { ...vscodeLock, lockfileVersion: 2 }, protocol],
    [new RegExp("public protocol"), vscodePackage, vscodeLock, { ...protocol, packageVersion: "9.9.9" }],
  ];
  for (const [pattern, packageManifest, lock, publicProtocol] of mutations) {
    assert.throws(
      () => validatePublicClientReleaseContract(version, packageManifest, lock, publicProtocol),
      pattern,
    );
  }
});

test("cross-runtime conformance manifestはrelease trainと一致する", () => {
  assert.doesNotThrow(() =>
    validateReleaseTrainVersions(plan.packageVersion, {
      "cross-runtime conformance manifest": conformance.packageVersion,
      "public conformance manifest": publicConformance.packageVersion,
    }));
  assert.throws(
    () =>
      validateReleaseTrainVersions(plan.packageVersion, {
        "cross-runtime conformance manifest": "9.9.9",
      }),
    /cross-runtime conformance manifest version/,
  );
  assert.throws(
    () =>
      validateReleaseTrainVersions(plan.packageVersion, {
        "public conformance manifest": "9.9.9",
      }),
    /public conformance manifest version/,
  );
});

test("公開WASM protocolは対応schemaを必須とする", () => {
  assert.doesNotThrow(() =>
    validatePublicClientReleaseContract(
      plan.packageVersion,
      vscodePackage,
      vscodeLock,
      protocol,
    ));
  assert.throws(
    () =>
      validatePublicClientReleaseContract(
        plan.packageVersion,
        vscodePackage,
        vscodeLock,
        { ...protocol, schemaVersion: SUPPORTED_PUBLIC_PROTOCOL_SCHEMA_VERSION - 1 },
      ),
    /public protocol schemaVersion must be 7/,
  );
});

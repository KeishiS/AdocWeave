import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { parseDistributionManifest, selectLspAsset } from "../src/distribution-manifest.js";
import { platformForHost } from "../src/platform.js";

const fixture = readFileSync("../../release/adocweave-dist-manifest.fixture.json", "utf8");

test("公開manifestからplatformに一致するassetを一意に選択します", () => {
  const manifest = parseDistributionManifest(fixture, "0.17.0");
  const asset = selectLspAsset(manifest, platformForHost("win32", "x64", "10.0.17763"));
  assert.equal(asset.name, "adocweave-lsp-x86_64-pc-windows-msvc.zip");
  assert.equal(asset.executable, "adocweave-lsp.exe");
});

test("未知field、version不一致、重複assetを拒否します", () => {
  const parsed = JSON.parse(fixture);
  assert.throws(
    () => parseDistributionManifest(JSON.stringify({ ...parsed, unknown: true }), "0.17.0"),
    /invalid-manifest/,
  );
  assert.throws(() => parseDistributionManifest(fixture, "9.9.9"), /invalid-manifest/);
  const windowsLsp = parsed.assets.find(
    ({ name }: { name: string }) => name === "adocweave-lsp-x86_64-pc-windows-msvc.zip",
  );
  const manifest = parseDistributionManifest(
    JSON.stringify({ ...parsed, assets: [...parsed.assets, windowsLsp] }),
    "0.17.0",
  );
  assert.throws(
    () => selectLspAsset(manifest, platformForHost("win32", "x64", "10.0.17763")),
    /asset-count/,
  );
});

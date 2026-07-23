import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { BROWSER_PACKAGE_VERSION, CONTRACT_VERSION } from "./contracts.mjs";

test("worker consumes the public WASM contract registry", async () => {
  const manifestUrl = new URL("../release-manifest.json", import.meta.url);
  const manifest = JSON.parse(await readFile(manifestUrl, "utf8"));

  assert.deepEqual(Object.keys(manifest).sort(), [
    "contractVersion",
    "packageVersion",
    "rustVersion",
    "schemaVersion",
  ]);
  assert.equal(manifest.schemaVersion, 2);
  assert.equal(manifest.contractVersion, CONTRACT_VERSION);
  assert.equal(manifest.packageVersion, BROWSER_PACKAGE_VERSION);
  assert.match(manifest.rustVersion, /^\d+\.\d+\.\d+$/);
});

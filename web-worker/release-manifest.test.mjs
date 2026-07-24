import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { BROWSER_PACKAGE_VERSION, PACKAGE_VERSION } from "./contracts.mjs";

test("worker consumes the public WASM contract registry", async () => {
  const manifestUrl = new URL("../release-manifest.json", import.meta.url);
  const manifest = JSON.parse(await readFile(manifestUrl, "utf8"));

  assert.deepEqual(Object.keys(manifest).sort(), [
    "packageVersion",
    "rustVersion",
    "schemaVersion",
  ]);
  assert.equal(manifest.schemaVersion, 3);
  assert.equal(manifest.packageVersion, PACKAGE_VERSION);
  assert.equal(manifest.packageVersion, BROWSER_PACKAGE_VERSION);
  assert.match(manifest.rustVersion, /^\d+\.\d+\.\d+$/);
});

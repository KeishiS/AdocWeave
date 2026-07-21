import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("worker consumes the public WASM contract registry", async () => {
  const manifestUrl = new URL("../release-manifest.json", import.meta.url);
  const manifest = JSON.parse(await readFile(manifestUrl, "utf8"));

  assert.deepEqual(Object.keys(manifest).sort(), [
    "contracts",
    "packageVersion",
    "schemaVersion",
  ]);
  assert.deepEqual(Object.keys(manifest.contracts).sort(), [
    "conformance",
    "coreApi",
    "coreProfile",
    "html",
    "projection",
    "wasmApi",
  ]);
  assert.equal(manifest.schemaVersion, 2);
  assert.equal(manifest.contracts.wasmApi, 5);
});

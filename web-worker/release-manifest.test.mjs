import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { WORKER_PROTOCOL_VERSION } from "./controller.mjs";

test("worker protocol matches the repository release manifest", async () => {
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
    "workerProtocol",
  ]);
  assert.equal(manifest.schemaVersion, 1);
  assert.equal(
    manifest.contracts.workerProtocol,
    WORKER_PROTOCOL_VERSION,
  );
});

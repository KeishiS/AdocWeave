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

test("READMEはBrowserのversion境界とprojection境界を説明する", async () => {
  const readme = await readFile(new URL("./README.adoc", import.meta.url), "utf8");

  assert.match(readme, /unsupported-package-version/);
  assert.match(readme, /Worker応答の.*version.*解析要求の.*version/s);
  assert.match(readme, /invalid-worker-response/);
  assert.match(readme, /staleな応答.*onResult.*onError.*通知しません/s);
  assert.match(readme, /onError.*microtask/s);
  assert.match(readme, /すべてのJSON objectで.*定義にないfieldを拒否/s);
});

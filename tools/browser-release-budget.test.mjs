import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_BROWSER_ARCHIVE_BYTES,
  MAX_BROWSER_WASM_BYTES,
  assertBrowserArtifactSizes,
  browserArtifactSizeError,
} from "./browser-release-budget.mjs";

test("browser performance budgets remain explicit release constants", () => {
  assert.equal(MAX_BROWSER_ARCHIVE_BYTES, 2_097_152);
  assert.equal(MAX_BROWSER_WASM_BYTES, 1_310_720);
});

test("browser archive and raw WASM accept their exact performance budgets", () => {
  assert.equal(
    browserArtifactSizeError(MAX_BROWSER_ARCHIVE_BYTES, MAX_BROWSER_WASM_BYTES),
    null,
  );
});

test("browser archive rejects the first byte beyond its performance budget", () => {
  assert.equal(
    browserArtifactSizeError(MAX_BROWSER_ARCHIVE_BYTES + 1, MAX_BROWSER_WASM_BYTES),
    `archive exceeds 2 MiB: ${MAX_BROWSER_ARCHIVE_BYTES + 1}`,
  );
});

test("raw browser WASM rejects the first byte beyond its performance budget", () => {
  assert.equal(
    browserArtifactSizeError(MAX_BROWSER_ARCHIVE_BYTES, MAX_BROWSER_WASM_BYTES + 1),
    `WASM exceeds 1.25 MiB: ${MAX_BROWSER_WASM_BYTES + 1}`,
  );
});

test("budget errors become release gate failures", () => {
  assert.throws(
    () => assertBrowserArtifactSizes(MAX_BROWSER_ARCHIVE_BYTES, MAX_BROWSER_WASM_BYTES + 1),
    /WASM exceeds 1\.25 MiB/,
  );
});

import assert from "node:assert/strict";
import test from "node:test";

import { platformForHost, supportedPlatforms } from "../src/platform.js";

test("共有fixtureの4 platformだけを選択します", () => {
  assert.equal(platformForHost("linux", "arm64", "6.12").target, "aarch64-unknown-linux-musl");
  assert.throws(() => platformForHost("darwin", "x64", "22.0"), /unsupported-platform/);
  assert.equal(platformForHost("win32", "x64", "10.0.17763").executable, "adocweave-lsp.exe");
  assert.equal(supportedPlatforms().length, 4);
});

test("未対応platformはdownload前に拒否します", () => {
  assert.throws(() => platformForHost("win32", "arm64"), /unsupported-platform/);
  assert.throws(() => platformForHost("linux", "ia32"), /unsupported-platform/);
  assert.throws(() => platformForHost("darwin", "arm64", "21.6"), /unsupported-os-version/);
  assert.throws(() => platformForHost("win32", "x64", "10.0.17762"), /unsupported-os-version/);
});

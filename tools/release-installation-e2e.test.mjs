import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const distributionPlan = JSON.parse(
  readFileSync(new URL("../release/distribution-plan.json", import.meta.url), "utf8"),
);
const target = distributionPlan.targets.find(({ architecture, os }) =>
  architecture === process.arch && os === process.platform);

function missingAsset(scope) {
  assert.ok(target, `unsupported test host: ${process.platform} ${process.arch}`);
  const candidate = mkdtempSync(join(tmpdir(), "adocweave-installation-missing-"));
  try {
    return spawnSync(
      process.execPath,
      [
        fileURLToPath(new URL("release-installation-e2e.mjs", import.meta.url)),
        candidate,
        target.triple,
        fileURLToPath(new URL("../release-manifest.json", import.meta.url)),
        scope,
      ],
      { encoding: "utf8" },
    );
  } finally {
    rmSync(candidate, { force: true, recursive: true });
  }
}

test("native-onlyは選択されたnative assetの欠落を拒否する", () => {
  const result = missingAsset("native-only");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /missing release asset: adocweave-cli-/);
});

test("global-onlyは選択されたglobal assetの欠落を拒否する", () => {
  const result = missingAsset("global-only");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /missing release asset: adocweave-browser-/);
});

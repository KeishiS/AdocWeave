import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, truncateSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

import { MAX_BROWSER_WASM_BYTES } from "./browser-release-budget.mjs";

test("browser smoke rejects an archive whose extracted raw WASM exceeds the budget", () => {
  const temporary = mkdtempSync(join(tmpdir(), "adocweave-browser-budget-"));
  try {
    const packageName = "adocweave-browser-budget-probe";
    const packageRoot = join(temporary, packageName);
    const wasmDirectory = join(packageRoot, "wasm");
    const archive = join(temporary, `${packageName}.tar.xz`);
    mkdirSync(wasmDirectory, { recursive: true });
    const wasm = join(wasmDirectory, "adocweave_wasm_bg.wasm");
    writeFileSync(wasm, "");
    truncateSync(wasm, MAX_BROWSER_WASM_BYTES + 1);
    const archived = spawnSync(
      "tar",
      ["-cJf", archive, "-C", temporary, packageName],
      { encoding: "utf8" },
    );
    assert.equal(archived.status, 0, archived.stderr);

    const checked = spawnSync(
      process.execPath,
      ["tools/browser-release-smoke.mjs", archive, process.execPath],
      { cwd: new URL("../", import.meta.url), encoding: "utf8" },
    );
    assert.notEqual(checked.status, 0);
    assert.match(checked.stderr, /WASM exceeds 1\.25 MiB: 1310721/);
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
});

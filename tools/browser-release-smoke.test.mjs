import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  truncateSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

import { MAX_BROWSER_WASM_BYTES } from "./browser-release-budget.mjs";
import {
  BROWSER_STARTUP_ATTEMPTS,
  BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS,
  BROWSER_STARTUP_TOTAL_TIMEOUT_MS,
  retryBrowserStartup,
} from "./browser-startup.mjs";

test("browser startup production bounds and process diagnostics remain explicit", () => {
  assert.equal(BROWSER_STARTUP_ATTEMPTS, 2);
  assert.equal(BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS, 20_000);
  assert.equal(BROWSER_STARTUP_TOTAL_TIMEOUT_MS, 45_000);

  const smoke = readFileSync(
    new URL("./browser-release-smoke.mjs", import.meta.url),
    "utf8",
  );
  assert.match(smoke, /profile-\$\{crypto\.randomUUID\(\)\}/);
  assert.match(smoke, /browser\.kill\("SIGTERM"\)/);
  assert.match(smoke, /browser\.kill\("SIGKILL"\)/);
  assert.match(smoke, /stderr = `\$\{stderr\}\$\{chunk\}`\.slice\(-8192\)/);
  assert.match(smoke, /browser exited before DevTools became ready \(\$\{status\}\)/);
});

test("browser startup aborts an in-flight attempt at the total deadline", async () => {
  const started = Date.now();
  const error = await retryBrowserStartup(
    ({ signal }) => new Promise((_, reject) => {
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    }),
    { attempts: 2, totalTimeoutMs: 20 },
  ).then(
    () => undefined,
    (failure) => failure,
  );

  assert.match(error.message, /20 ms total timeout/);
  assert.ok(Date.now() - started < 500);
});

test("browser startup retries once with an attempt-specific diagnostic", async () => {
  const attempts = [];
  const failures = [];
  let clock = 100;
  const result = await retryBrowserStartup(
    async ({ attempt }) => {
      attempts.push(attempt);
      if (attempt === 1) {
        clock = 125;
        const error = new Error("DevToolsActivePort timeout");
        error.retryBrowserStartup = true;
        throw error;
      }
      return "ready";
    },
    {
      attempts: 2,
      totalTimeoutMs: 1000,
      onFailure: (failure) => failures.push(failure),
      now: () => clock,
    },
  );

  assert.equal(result, "ready");
  assert.deepEqual(attempts, [1, 2]);
  assert.equal(failures.length, 1);
  assert.equal(failures[0].attempt, 1);
  assert.equal(failures[0].attempts, 2);
  assert.equal(failures[0].elapsedMs, 25);
  assert.equal(failures[0].willRetry, true);
  assert.match(failures[0].error.message, /DevToolsActivePort timeout/);
});

test("browser startup retry respects the total deadline", async () => {
  let clock = 0;
  const failures = [];
  const error = await retryBrowserStartup(
    async ({ remainingMs }) => {
      assert.equal(remainingMs, 30);
      clock = 30;
      const failure = new Error("cold start");
      failure.retryBrowserStartup = true;
      throw failure;
    },
    {
      attempts: 2,
      totalTimeoutMs: 30,
      now: () => clock,
      onFailure: (failure) => failures.push(failure),
    },
  ).then(
    () => undefined,
    (failure) => failure,
  );

  assert.match(error.message, /cold start/);
  assert.equal(failures.length, 1);
  assert.equal(failures[0].elapsedMs, 30);
  assert.equal(failures[0].willRetry, false);
});

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

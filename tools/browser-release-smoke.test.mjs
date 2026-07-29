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
import { EventEmitter } from "node:events";
import test from "node:test";

import { MAX_BROWSER_WASM_BYTES } from "./browser-release-budget.mjs";
import { inspectPage } from "./browser-release-smoke.mjs";
import {
  BROWSER_STARTUP_ATTEMPTS,
  BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS,
  BROWSER_STARTUP_TOTAL_TIMEOUT_MS,
  retryBrowserStartup,
} from "./browser-startup.mjs";

test("browser startup production bounds and process diagnostics remain explicit", () => {
  assert.equal(BROWSER_STARTUP_ATTEMPTS, 3);
  assert.equal(BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS, 20_000);
  assert.equal(BROWSER_STARTUP_TOTAL_TIMEOUT_MS, 75_000);

  const smoke = readFileSync(
    new URL("./browser-release-smoke.mjs", import.meta.url),
    "utf8",
  );
  assert.match(smoke, /profile-\$\{randomUUID\(\)\}/);
  assert.match(smoke, /browser\.kill\("SIGTERM"\)/);
  assert.match(smoke, /browser\.kill\("SIGKILL"\)/);
  assert.match(smoke, /stderr = `\$\{stderr\}\$\{chunk\}`\.slice\(-8192\)/);
  assert.match(smoke, /browser exited before DevTools became ready \(\$\{status\}\)/);
  assert.match(smoke, /withAbortSignal\(call\("Page\.enable"\), startupSignal\)/);
  assert.match(smoke, /during \$\{startupPhase\}/);
  assert.match(smoke, /Page\.navigate"[\s\S]*5000, "Page\.navigate timeout"/);
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
  assert.match(error.message, /exhausted 1\/2 attempts/);
  assert.equal(failures.length, 1);
  assert.equal(failures[0].elapsedMs, 30);
  assert.equal(failures[0].willRetry, false);
});

test("Page.enable timeout replaces the profile, process, and socket", async () => {
  const harness = browserHarness({ firstPageEnableHangs: true });
  const result = await inspectPage(
    "chromium",
    "http://example.test",
    "/temporary",
    {
      attempts: 2,
      attemptTimeoutMs: 10,
      totalTimeoutMs: 5000,
      dependencies: harness.dependencies,
    },
  );

  assert.equal(result.status, "ready:4:5");
  assert.deepEqual(harness.profiles, [
    "/temporary/profile-profile-1",
    "/temporary/profile-profile-2",
  ]);
  assert.equal(harness.browsers.length, 2);
  assert.deepEqual(
    harness.browsers.map((browser) => browser.kills),
    [["SIGTERM"], ["SIGTERM"]],
  );
  assert.equal(harness.sockets.length, 2);
  assert.ok(harness.sockets.every((socket) => socket.readyState === 3));
});

test("Page.navigate failure is not retried", async () => {
  const harness = browserHarness({ navigationFails: true });
  const error = await inspectPage(
    "chromium",
    "http://example.test",
    "/temporary",
    {
      attempts: 3,
      attemptTimeoutMs: 20,
      totalTimeoutMs: 5000,
      dependencies: harness.dependencies,
    },
  ).then(
    () => undefined,
    (failure) => failure,
  );

  assert.match(error.message, /navigation rejected/);
  assert.equal(harness.browsers.length, 1);
  assert.equal(harness.sockets.length, 1);
});

test("a stalled DevToolsActivePort read obeys the attempt deadline", async () => {
  const harness = browserHarness();
  harness.dependencies.readText = async () => new Promise(() => {});
  const started = Date.now();
  const error = await inspectPage(
    "chromium",
    "http://example.test",
    "/temporary",
    {
      attempts: 1,
      attemptTimeoutMs: 10,
      totalTimeoutMs: 5000,
      dependencies: harness.dependencies,
    },
  ).then(
    () => undefined,
    (failure) => failure,
  );

  assert.match(error.message, /during DevToolsActivePort/);
  assert.ok(Date.now() - started < 2000);
  assert.deepEqual(harness.browsers[0].kills, ["SIGTERM"]);
});

test("browser startup exhaustion reports every attempted handshake", async () => {
  const error = await retryBrowserStartup(
    async ({ attempt }) => {
      const failure = new Error(`handshake ${attempt}`);
      failure.retryBrowserStartup = true;
      throw failure;
    },
    { attempts: 3, totalTimeoutMs: 1000 },
  ).then(
    () => undefined,
    (failure) => failure,
  );

  assert.match(error.message, /exhausted 3\/3 attempts/);
  assert.match(error.message, /attempt 1: handshake 1/);
  assert.match(error.message, /attempt 2: handshake 2/);
  assert.match(error.message, /attempt 3: handshake 3/);
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

function browserHarness({
  firstPageEnableHangs = false,
  navigationFails = false,
} = {}) {
  const browsers = [];
  const profiles = [];
  const sockets = [];
  let profileNumber = 0;

  class FakeWebSocket extends EventTarget {
    static CLOSING = 2;

    constructor() {
      super();
      this.readyState = 0;
      this.attempt = sockets.length + 1;
      sockets.push(this);
      queueMicrotask(() => {
        this.readyState = 1;
        this.dispatchEvent(new Event("open"));
      });
    }

    send(payload) {
      const request = JSON.parse(payload);
      if (
        request.method === "Page.enable"
        && firstPageEnableHangs
        && this.attempt === 1
      ) {
        return;
      }
      if (request.method === "Page.navigate" && navigationFails) {
        this.reply({
          id: request.id,
          error: { message: "navigation rejected" },
        });
        return;
      }
      this.reply({ id: request.id, result: evaluationResult(request.method) });
      if (request.method === "Page.navigate") {
        this.reply({ method: "Page.loadEventFired", params: {} });
      }
    }

    reply(message) {
      queueMicrotask(() => this.dispatchEvent(new MessageEvent(
        "message",
        { data: JSON.stringify(message) },
      )));
    }

    close() {
      this.readyState = 3;
    }
  }

  return {
    browsers,
    profiles,
    sockets,
    dependencies: {
      randomUUID: () => `profile-${++profileNumber}`,
      spawnBrowser: (_command, args) => {
        profiles.push(
          args.find((argument) => argument.startsWith("--user-data-dir="))
            .slice("--user-data-dir=".length),
        );
        const browser = new EventEmitter();
        browser.stderr = new EventEmitter();
        browser.stderr.setEncoding = () => {};
        browser.exitCode = null;
        browser.signalCode = null;
        browser.kills = [];
        browser.kill = (signal) => browser.kills.push(signal);
        browsers.push(browser);
        return browser;
      },
      readText: async () => "9222\n",
      fetchTarget: async () => ({
        json: async () => [{
          type: "page",
          webSocketDebuggerUrl: "ws://fake.test",
        }],
      }),
      WebSocketImplementation: FakeWebSocket,
      waitForBrowserExit: async () => true,
    },
  };
}

function evaluationResult(method) {
  if (method !== "Runtime.evaluate") return {};
  return {
    result: {
      value: {
        status: "ready:4:5",
        html: "Latest browser result",
        isolated: false,
        packageVersion: "test-version",
        resultPackageVersion: "test-version",
        wasmPackageVersion: "test-version",
        analysisPackageVersion: "test-version",
        projectionPackageVersion: "test-version",
      },
    },
  };
}

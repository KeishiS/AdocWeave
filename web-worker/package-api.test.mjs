import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";

import {
  AdocWeaveClient,
  BROWSER_PACKAGE_VERSION,
  defaultAssetUrls,
} from "./index.mjs";

test("public entry owns worker and WASM asset resolution", () => {
  const urls = defaultAssetUrls("https://example.test/pkg/worker/index.mjs");
  assert.equal(urls.workerUrl.href, "https://example.test/pkg/worker/worker.mjs");
  assert.equal(urls.moduleUrl.href, "https://example.test/pkg/wasm/adocweave_wasm.js");
  assert.equal(typeof AdocWeaveClient, "function");
  assert.match(BROWSER_PACKAGE_VERSION, /^\d+\.\d+\.\d+(?:-rc\.[1-9]\d*)?$/);
});

test("package metadata exposes only the typed public entry", async () => {
  const pkg = JSON.parse(await readFile(new URL("./package.json", import.meta.url)));
  assert.equal(pkg.name, "@adocweave/browser");
  assert.equal(pkg.version, BROWSER_PACKAGE_VERSION);
  assert.deepEqual(pkg.exports["."], {
    types: "./worker/index.d.mts",
    import: "./worker/index.mjs",
  });
});

test("fallback mode recreates workers and never publishes stale results", async () => {
  const workers = [];
  class FakeWorker {
    listeners = new Map();
    terminated = false;
    constructor() { workers.push(this); }
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({ data: { type: "ready" } }));
      }
      this.lastMessage = message;
    }
    terminate() { this.terminated = true; }
    publish(data) { this.listeners.get("message")?.({ data }); }
  }
  const results = [];
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: false,
    onResult: (result) => results.push(result),
  });
  client.update({ version: 1, source: "old" });
  const oldWorker = workers.at(-1);
  client.update({ version: 2, source: "new" });
  const currentWorker = workers.at(-1);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(oldWorker.terminated, true);
  oldWorker.publish({ type: "result", version: 1, generation: 1, result: { html: "old" } });
  currentWorker.publish({ type: "result", version: 2, generation: 2, result: {
    apiVersion: 5, html: "new", diagnostics: [], renderDiagnostics: [], contractVersion: 5,
  } });
  assert.equal(results.length, 1);
  assert.equal(results[0].html, "new");
  assert.equal(results[0].sourceVersion, 2);
  client.dispose();
});

test("client rejects a WASM result with a different contract version", async () => {
  const errors = [];
  const workers = [];
  class FakeWorker {
    listeners = new Map();
    constructor() { workers.push(this); }
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({ data: { type: "ready" } }));
      }
      this.lastMessage = message;
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: false,
    onError: (error) => errors.push(error),
  });
  client.update({ version: 1, source: "text" });
  await new Promise((resolve) => setTimeout(resolve, 0));
  workers.at(-1).publish({
    type: "result",
    version: 1,
    generation: 1,
    result: { apiVersion: 1 },
  });
  assert.deepEqual(errors, [{
    code: "unsupported-contract-version",
    message: "expected contract version 5",
    sourceVersion: 1,
    generation: 1,
  }]);
  client.dispose();
});

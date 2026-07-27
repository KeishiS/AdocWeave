import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";

import {
  AdocWeaveClient,
  BROWSER_PACKAGE_VERSION,
  defaultAssetUrls,
} from "./index.mjs";
import { WORKER_PROTOCOL_VERSION } from "./contracts.mjs";

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
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
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
  oldWorker.publish(responseEnvelope(1, 1, BROWSER_PACKAGE_VERSION, "old"));
  currentWorker.publish(responseEnvelope(2, 2, BROWSER_PACKAGE_VERSION, "new"));
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
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
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
  workers.at(-1).publish(responseEnvelope(1, 1, "0.0.1", ""));
  assert.deepEqual(errors, [{
    code: "unsupported-package-version",
    message: `expected package version ${BROWSER_PACKAGE_VERSION}`,
    sourceVersion: 1,
    generation: 1,
  }]);
  client.dispose();
});

test("client rejects and terminates an obsolete worker protocol during initialization", async () => {
  const errors = [];
  const messages = [];
  class FakeWorker {
    listeners = new Map();
    terminated = false;
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      messages.push(message);
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION - 1, type: "ready" },
        }));
      }
    }
    terminate() { this.terminated = true; }
  }
  const worker = new FakeWorker();
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs",
    moduleUrl: "wasm.js",
    wasmUrl: "wasm.wasm",
    Worker: class {
      constructor() { return worker; }
    },
    sharedCancellation: true,
    onError: (error) => errors.push(error),
  });
  client.update({ version: 1, source: "text" });
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.equal(worker.terminated, true);
  assert.equal(messages.length, 1);
  assert.deepEqual(errors, [{
    code: "unsupported-worker-protocol",
    message: `expected worker protocol ${WORKER_PROTOCOL_VERSION}`,
    sourceVersion: null,
    generation: 1,
  }]);
  client.dispose();
});

function responseEnvelope(version, generation, packageVersion, html) {
  return {
    protocolVersion: WORKER_PROTOCOL_VERSION,
    type: "result",
    version,
    generation,
    result: {
      packageVersion,
      version,
      generation,
      products: {
        syntax: false,
        canonicalAst: false,
        html: true,
        attributeOccurrences: false,
        attributeQueries: false,
        resourceQueries: false,
        diagnostics: true,
        symbols: false,
        projection: false,
      },
      parse: {
        packageVersion,
        blockCount: 0,
        nodeCount: 0,
        referenceCount: 0,
      },
      syntax: "",
      ast: "",
      html,
      attributeOccurrences: [],
      attributeQueries: { bindings: [], references: [] },
      resourceQueries: [],
      diagnostics: [],
      renderDiagnostics: [],
      symbols: [],
      projection: {
        packageVersion,
        sourceId: null,
        sourceBlocks: [],
        formulas: [],
        blockPresentations: [],
        orderedLists: [],
        referenceEdges: [],
        externalLinks: [],
        searchableText: { text: "", segments: [] },
        structure: { headings: [], toc: [], manpage: null },
        catalogs: { footnotes: [], bibliography: [], index: [] },
        targets: [],
        title: null,
      },
    },
  };
}

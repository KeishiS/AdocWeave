import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";

import {
  AdocWeaveClient,
  AdocWeaveClientError,
  BROWSER_PACKAGE_VERSION,
  analyzeOnce,
  defaultAssetUrls,
  isAdocWeaveClientLifecycleError,
} from "./index.mjs";
import { WORKER_PROTOCOL_VERSION } from "./contracts.mjs";

test("public entry owns worker and WASM asset resolution", () => {
  for (const base of [
    "https://example.test/pkg/worker/index.mjs",
    "https://example.test/assets/adocweave/worker/index.mjs?hash=vite",
    "https://cdn.example.test/webpack/adocweave/worker/index.mjs",
  ]) {
    const urls = defaultAssetUrls(base);
    const root = new URL("../", base);
    assert.equal(urls.workerUrl.href, new URL("worker/worker.mjs", root).href);
    assert.equal(urls.moduleUrl.href, new URL("wasm/adocweave_wasm.js", root).href);
    assert.equal(urls.wasmUrl.href, new URL("wasm/adocweave_wasm_bg.wasm", root).href);
  }
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

test("ready and analyze provide a one-shot Promise over the callback controller", async () => {
  const workers = [];
  const callbackResults = [];
  class FakeWorker {
    listeners = new Map();
    constructor() { workers.push(this); }
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.publish({
          protocolVersion: WORKER_PROTOCOL_VERSION,
          type: "ready",
        }));
      } else {
        queueMicrotask(() => this.publish(
          responseEnvelope(message.version, message.generation, BROWSER_PACKAGE_VERSION, "one-shot"),
        ));
      }
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: false,
    onResult: (result) => callbackResults.push(result),
  });

  assert.equal(workers.length, 0);
  await client.ready;
  assert.equal(workers.length, 1);
  const result = await client.analyze({ version: 1, source: "text" });
  assert.equal(result.html, "one-shot");
  assert.equal(result.sourceVersion, 1);
  assert.equal(Object.hasOwn(result, "version"), false);
  assert.equal(Object.hasOwn(result, "result"), false);
  assert.equal(callbackResults[0], result);
  client.dispose();
});

test("one-shot Promise rejects supersede, cancel, and dispose with stable codes", async () => {
  class FakeWorker {
    listeners = new Map();
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
      }
    }
    terminate() {}
  }
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: true,
  });

  const first = client.analyze({ version: 1, source: "first" });
  const firstRejected = assert.rejects(first, errorWithCode("superseded"));
  const second = client.analyze({ version: 2, source: "second" });
  await firstRejected;
  const secondRejected = assert.rejects(second, errorWithCode("cancelled"));
  client.cancel();
  await secondRejected;
  const third = client.analyze({ version: 3, source: "third" });
  const thirdRejected = assert.rejects(third, errorWithCode("disposed"));
  client.dispose();
  await thirdRejected;
});

test("callback exceptions never prevent Promise settlement", async () => {
  class FakeWorker {
    listeners = new Map();
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.publish({
          protocolVersion: WORKER_PROTOCOL_VERSION,
          type: "ready",
        }));
      } else {
        queueMicrotask(() => this.publish(
          responseEnvelope(message.version, message.generation, BROWSER_PACKAGE_VERSION, "settled"),
        ));
      }
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: false,
    onResult() { throw new Error("callback failed"); },
  });
  const result = await client.analyze({ version: 1, source: "text" });
  assert.equal(result.html, "settled");
  await new Promise((resolve) => setTimeout(resolve, 0));
  const next = await client.analyze({ version: 2, source: "next" });
  assert.equal(next.html, "settled");
  client.dispose();
});

test("synchronous worker failures reject the already registered analysis", async () => {
  class ProtocolMismatchWorker {
    listeners = new Map();
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION - 1, type: "ready" },
        });
      }
    }
    terminate() {}
  }
  const mismatch = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: ProtocolMismatchWorker, sharedCancellation: false,
  });
  await assert.rejects(
    mismatch.analyze({ version: 1, source: "text" }),
    errorWithCode("unsupported-worker-protocol"),
  );
  mismatch.dispose();

  const readyMismatch = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: ProtocolMismatchWorker, sharedCancellation: false,
  });
  await assert.rejects(
    readyMismatch.ready,
    errorWithCode("unsupported-worker-protocol"),
  );
  readyMismatch.dispose();

  const constructorFailure = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: class { constructor() { throw new Error("constructor failed"); } },
    sharedCancellation: false,
  });
  await assert.rejects(
    constructorFailure.analyze({ version: 1, source: "text" }),
    errorWithCode("worker-failed"),
  );
  constructorFailure.dispose();

  class PostMessageFailureWorker {
    addEventListener() {}
    postMessage() { throw new Error("postMessage failed"); }
    terminate() {}
  }
  const postMessageFailure = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: PostMessageFailureWorker, sharedCancellation: false,
  });
  await assert.rejects(
    postMessageFailure.analyze({ version: 1, source: "text" }),
    errorWithCode("worker-failed"),
  );
  postMessageFailure.dispose();
});

test("constructor failure callbacks can retry without losing the new version contract", async () => {
  const results = [];
  const workers = [];
  let attempts = 0;
  class RetryWorker {
    listeners = new Map();
    constructor() {
      ++attempts;
      if (attempts === 1) throw new Error("constructor failed");
      workers.push(this);
    }
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
      }
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }
  let client;
  client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: RetryWorker, sharedCancellation: false,
    onError(error) {
      assert.equal(error.code, "worker-failed");
      client.update({ version: 2, source: "retry" });
    },
    onResult: (result) => results.push(result),
  });

  const failed = client.analyze({ version: 1, source: "first" });
  await assert.rejects(failed, errorWithCode("worker-failed"));
  await new Promise((resolve) => setTimeout(resolve, 0));
  workers[0].publish(responseEnvelope(2, 2, BROWSER_PACKAGE_VERSION, "retried"));

  assert.equal(results[0].html, "retried");
  assert.equal(results[0].sourceVersion, 2);
  assert.equal(results.length, 1);
  client.dispose();
});

test("consecutive constructor failures retry without synchronous recursion", async () => {
  const results = [];
  const workers = [];
  let attempts = 0;
  class RetryWorker {
    listeners = new Map();
    constructor() {
      ++attempts;
      if (attempts <= 3) throw new Error(`constructor failed ${attempts}`);
      workers.push(this);
    }
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
      }
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }
  let client;
  client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: RetryWorker, sharedCancellation: false,
    onError() {
      if (attempts <= 3) {
        client.update({ version: attempts + 1, source: "retry" });
      }
    },
    onResult: (result) => results.push(result),
  });

  await assert.rejects(
    client.analyze({ version: 1, source: "first" }),
    errorWithCode("worker-failed"),
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(attempts, 4);
  workers[0].publish(responseEnvelope(4, 4, BROWSER_PACKAGE_VERSION, "retried"));
  assert.equal(results[0].sourceVersion, 4);
  client.dispose();
});

test("terminal worker failures can retry without terminating the replacement worker", async () => {
  for (const failure of ["identity", "worker-error"]) {
    const results = [];
    const workers = [];
    class RetryWorker {
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
      }
      terminate() { this.terminated = true; }
      publish(data) { this.listeners.get("message")?.({ data }); }
      fail() { this.listeners.get("error")?.({ message: "worker failed" }); }
    }
    let client;
    client = new AdocWeaveClient({
      workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
      Worker: RetryWorker, sharedCancellation: false,
      onError() {
        client.update({ version: 2, source: "retry" });
      },
      onResult: (result) => results.push(result),
    });

    const first = client.analyze({ version: 1, source: "first" });
    const rejected = assert.rejects(
      first,
      errorWithCode(failure === "identity"
        ? "invalid-worker-response"
        : "worker-failed"),
    );
    await new Promise((resolve) => setTimeout(resolve, 0));
    if (failure === "identity") {
      workers[0].publish(responseEnvelope(9, 1, BROWSER_PACKAGE_VERSION, "invalid"));
    } else {
      workers[0].fail();
    }
    await rejected;
    await new Promise((resolve) => setTimeout(resolve, 0));

    assert.equal(workers[0].terminated, true);
    assert.equal(workers[1].terminated, false);
    workers[1].publish(responseEnvelope(2, 2, BROWSER_PACKAGE_VERSION, "retried"));
    assert.equal(results[0].sourceVersion, 2);
    client.dispose();
  }
});

test("disposed analyze is a typed rejected Promise", async () => {
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: class {}, sharedCancellation: false,
  });
  client.dispose();
  const analysis = client.analyze({ version: 1, source: "text" });
  assert.equal(typeof analysis.then, "function");
  await assert.rejects(analysis, (error) =>
    isAdocWeaveClientLifecycleError(error) && error.code === "disposed");
});

test("fallback analyze supersedes an unfinished ready initialization", async () => {
  class FakeWorker {
    addEventListener() {}
    postMessage() {}
    terminate() {}
  }
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: false,
  });
  const ready = client.ready;
  const rejectedReady = assert.rejects(ready, errorWithCode("superseded"));
  const analysis = client.analyze({ version: 1, source: "text" });
  await rejectedReady;
  client.cancel();
  await assert.rejects(analysis, errorWithCode("cancelled"));
  client.dispose();
});

test("SSR import and client construction are lazy until ready or analyze", async () => {
  const originalWorker = globalThis.Worker;
  const originalFetch = globalThis.fetch;
  let networkRequests = 0;
  try {
    globalThis.Worker = undefined;
    globalThis.fetch = async () => {
      ++networkRequests;
      throw new Error("unexpected network request");
    };
    const imported = await import(`./index.mjs?ssr=${Date.now()}`);
    assert.equal(typeof imported.AdocWeaveClient, "function");
    assert.equal(networkRequests, 0);
  } finally {
    globalThis.Worker = originalWorker;
    globalThis.fetch = originalFetch;
  }

  let workers = 0;
  let wasmInitializations = 0;
  class ProbeWorker {
    listeners = new Map();
    constructor() { ++workers; }
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        ++wasmInitializations;
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
      }
    }
    terminate() {}
  }
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: ProbeWorker, sharedCancellation: false,
  });
  assert.equal(workers, 0);
  assert.equal(wasmInitializations, 0);
  await client.ready;
  assert.equal(workers, 1);
  assert.equal(wasmInitializations, 1);
  client.dispose();
});

test("analyzeOnce is a framework-independent one-shot utility", async () => {
  let terminated = false;
  class FakeWorker {
    listeners = new Map();
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      queueMicrotask(() => this.listeners.get("message")?.({
        data: message.type === "initialize"
          ? { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" }
          : responseEnvelope(message.version, message.generation, BROWSER_PACKAGE_VERSION, "once"),
      }));
    }
    terminate() { terminated = true; }
  }
  const result = await analyzeOnce({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: false,
  }, { version: 1, source: "text" });
  assert.equal(result.html, "once");
  assert.equal(terminated, true);
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
  const analysis = client.analyze({ version: 1, source: "text" });
  const rejected = assert.rejects(
    analysis,
    errorWithCode("unsupported-package-version"),
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  workers.at(-1).publish(responseEnvelope(1, 1, "0.0.1", ""));
  await rejected;
  assert.deepEqual(errors, [{
    code: "unsupported-package-version",
    message: `expected package version ${BROWSER_PACKAGE_VERSION}`,
    sourceVersion: 1,
    generation: 1,
  }]);
  client.dispose();
});

test("client rejects result identities which disagree with the worker envelope", async () => {
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
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }

  for (const field of ["version", "generation"]) {
    const errors = [];
    const client = new AdocWeaveClient({
      workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
      Worker: FakeWorker, sharedCancellation: false,
      onError: (error) => errors.push(error),
    });
    const analysis = client.analyze({ version: 1, source: "text" });
    const rejected = assert.rejects(analysis, errorWithCode("invalid-worker-response"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    const response = responseEnvelope(1, 1, BROWSER_PACKAGE_VERSION, "");
    response.result[field] = 2;
    workers.at(-1).publish(response);
    await rejected;
    assert.equal(errors.at(-1).code, "invalid-worker-response", field);
    client.dispose();
  }
});

test("client rejects result and error versions which disagree with the request", async () => {
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
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }

  for (const response of [
    () => responseEnvelope(2, 1, BROWSER_PACKAGE_VERSION, ""),
    () => ({
      protocolVersion: WORKER_PROTOCOL_VERSION,
      type: "error",
      version: 2,
      generation: 1,
      error: { code: "cancelled", message: "cancelled" },
    }),
  ]) {
    const client = new AdocWeaveClient({
      workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
      Worker: FakeWorker, sharedCancellation: false,
    });
    const analysis = client.analyze({ version: 1, source: "text" });
    const rejected = assert.rejects(analysis, errorWithCode("invalid-worker-response"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    workers.at(-1).publish(response());
    await rejected;
    client.dispose();
  }
});

test("stale responses cannot disturb the current request version contract", async () => {
  const results = [];
  const errors = [];
  let worker;
  class FakeWorker {
    listeners = new Map();
    constructor() { worker = this; }
    addEventListener(type, callback) { this.listeners.set(type, callback); }
    postMessage(message) {
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
      }
    }
    terminate() {}
    publish(data) { this.listeners.get("message")?.({ data }); }
  }
  const client = new AdocWeaveClient({
    workerUrl: "worker.mjs", moduleUrl: "wasm.js", wasmUrl: "wasm.wasm",
    Worker: FakeWorker, sharedCancellation: true,
    onResult: (result) => results.push(result),
    onError: (error) => errors.push(error),
  });
  assert.equal(client.update({ version: 1, source: "old" }), 1);
  assert.equal(client.update({ version: 2, source: "current" }), 2);
  await new Promise((resolve) => setTimeout(resolve, 0));

  worker.publish(responseEnvelope(99, 1, BROWSER_PACKAGE_VERSION, "stale"));
  worker.publish(responseEnvelope(2, 2, BROWSER_PACKAGE_VERSION, "current"));

  assert.equal(results[0].html, "current");
  assert.equal(results[0].sourceVersion, 2);
  assert.equal(results.length, 1);
  assert.deepEqual(errors, []);
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
  const analysis = client.analyze({ version: 1, source: "text" });
  const rejected = assert.rejects(
    analysis,
    errorWithCode("unsupported-worker-protocol"),
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  await rejected;

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
      projection: null,
    },
  };
}

function errorWithCode(code) {
  return (error) => error instanceof AdocWeaveClientError && error.code === code;
}

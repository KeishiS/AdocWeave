import assert from "node:assert/strict";
import test from "node:test";

import {
  WORKER_PROTOCOL_VERSION,
  createController,
} from "./controller.mjs";
import { AdocWeaveWorkerClient } from "./client.mjs";
import { PACKAGE_VERSION } from "./contracts.mjs";
import {
  WORKER_MESSAGE_FIELDS,
  WORKER_PROTOCOL_VERSION as GENERATED_WORKER_PROTOCOL_VERSION,
  validateClientError,
  validateWorkerMessage,
} from "./protocol.generated.mjs";

function harness(process = (request) => request) {
  const messages = [];
  const scheduled = new Map();
  let nextId = 0;
  const cancellation = new Int32Array(new SharedArrayBuffer(4));
  const controller = createController({
    process,
    publish: (message) => messages.push(message),
    isCurrent: (generation) => Atomics.load(cancellation, 0) === generation,
    schedule(callback) {
      const id = ++nextId;
      scheduled.set(id, callback);
      return id;
    },
    unschedule(id) {
      scheduled.delete(id);
    },
  });
  return {
    controller,
    messages,
    cancellation,
    flush() {
      const callbacks = [...scheduled.values()];
      scheduled.clear();
      callbacks.forEach((callback) => callback());
    },
  };
}

function request(version, generation) {
  return {
    protocolVersion: WORKER_PROTOCOL_VERSION,
    type: "analyze",
    version,
    generation,
    payload: {
      packageVersion: PACKAGE_VERSION,
      sourceId: null,
      version,
      generation,
      source: `version ${version}`,
    },
  };
}

function assertMessageFields(message, contract) {
  assert.deepEqual(Object.keys(message).sort(), [...WORKER_MESSAGE_FIELDS[contract]].sort());
}

function assertWorkerContract(message, direction) {
  assert.equal(validateWorkerMessage(message, direction), true);
  assert.equal(validateWorkerMessage({ ...message, unexpected: true }, direction), false);
  for (const field of Object.keys(message)) {
    const missing = { ...message };
    delete missing[field];
    assert.equal(validateWorkerMessage(missing, direction), false, `missing ${field}`);
  }
  for (const [path, invalid] of invalidNestedValues(message)) {
    const mutated = structuredClone(message);
    let target = mutated;
    for (const segment of path.slice(0, -1)) target = target[segment];
    target[path.at(-1)] = invalid;
    assert.equal(
      validateWorkerMessage(mutated, direction),
      false,
      `invalid nested value at ${path.join(".")}`,
    );
  }
}

function invalidNestedValues(value, path = []) {
  const mutations = [];
  for (const [key, child] of Object.entries(value)) {
    const childPath = [...path, key];
    if (typeof child === "string") mutations.push([childPath, false]);
    else if (typeof child === "number") mutations.push([childPath, "invalid"]);
    else if (typeof child === "boolean") mutations.push([childPath, "invalid"]);
    else if (child === null) mutations.push([childPath, false]);
    else if (Array.isArray(child)) mutations.push([childPath, "invalid"]);
    else if (typeof child === "object") {
      mutations.push([childPath, { ...child, unexpected: true }]);
      mutations.push(...invalidNestedValues(child, childPath));
    }
  }
  return mutations;
}

test("runtime uses the generated worker protocol version", () => {
  assert.equal(WORKER_PROTOCOL_VERSION, GENERATED_WORKER_PROTOCOL_VERSION);
});

test("worker ready envelope matches the generated contract", async () => {
  const previousSelf = globalThis.self;
  const messages = [];
  globalThis.self = {
    postMessage: (message) => messages.push(message),
  };
  try {
    await import(`./worker.mjs?protocol-contract=${Date.now()}`);
    await globalThis.self.onmessage({
      data: {
        protocolVersion: WORKER_PROTOCOL_VERSION,
        type: "initialize",
        moduleUrl: "data:text/javascript,export default async function init(){};export function process(){}",
        wasmUrl: "unused.wasm",
        debounceMs: 0,
        cancellationBuffer: null,
      },
    });
    assertMessageFields(messages[0], "responses.ready");
    assertWorkerContract(messages[0], "responses");
  } finally {
    globalThis.self = previousSelf;
  }
});

test("debounce publishes only the latest document generation", () => {
  const state = harness();
  Atomics.store(state.cancellation, 0, 1);
  state.controller.submit(request(1, 1));
  Atomics.store(state.cancellation, 0, 2);
  state.controller.submit(request(2, 2));
  state.flush();

  assert.equal(state.messages.length, 1);
  assertMessageFields(state.messages[0], "responses.result");
  assert.equal(state.messages[0].version, 2);
  assert.equal(state.messages[0].generation, 2);
});

test("shared generation cancels synchronous WASM cooperatively", () => {
  let observedCancellation = false;
  const state = harness((_request, isCancelled) => {
    Atomics.store(state.cancellation, 0, 2);
    observedCancellation = isCancelled();
    throw JSON.stringify({ code: "cancelled", message: "cancelled" });
  });
  Atomics.store(state.cancellation, 0, 1);
  state.controller.submit(request(1, 1));
  state.flush();

  assert.equal(observedCancellation, true);
  assert.deepEqual(state.messages, []);
});

test("protocol mismatch returns a stable error without executing WASM", () => {
  let calls = 0;
  const state = harness(() => {
    calls += 1;
  });
  state.controller.submit({ ...request(1, 1), protocolVersion: 2 });

  assert.equal(calls, 0);
  assertMessageFields(state.messages[0], "responses.error");
  assertWorkerContract(state.messages[0], "responses");
  assert.equal(state.messages[0].error.code, "unsupported-worker-protocol");
});

test("client sends the current WASM API version with responsibility-specific defaults", async () => {
  const messages = [];
  class FakeWorker {
    listeners = new Map();
    postMessage(message) {
      messages.push(message);
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({
          data: { protocolVersion: WORKER_PROTOCOL_VERSION, type: "ready" },
        }));
      }
    }
    addEventListener(type, listener) { this.listeners.set(type, listener); }
    terminate() {}
  }
  const client = new AdocWeaveWorkerClient({
    workerUrl: "worker.js",
    moduleUrl: "module.js", wasmUrl: "module.wasm", Worker: FakeWorker,
    sharedCancellation: true,
  });
  client.update({ version: 1, source: "text" });

  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.equal(messages[1].payload.packageVersion, PACKAGE_VERSION);
  assertMessageFields(messages[0], "requests.initialize");
  assertMessageFields(messages[1], "requests.analyze");
  assertWorkerContract(messages[0], "requests");
  assertWorkerContract(messages[1], "requests");
  assert.equal(validateWorkerMessage({
    ...messages[1],
    protocolVersion: String(WORKER_PROTOCOL_VERSION),
  }, "requests"), false);
  for (const invalid of [-1, 1.5, 4294967296, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.equal(validateWorkerMessage({
      ...messages[1],
      protocolVersion: invalid,
    }, "requests"), false);
  }
  assert.equal(validateWorkerMessage({
    ...messages[1],
    payload: { ...messages[1].payload, source: false },
  }, "requests"), false);
  assert.deepEqual(messages[1].payload.analysisOptions, {});
  assert.deepEqual(messages[1].payload.renderPolicy, {});
  assert.deepEqual(messages[1].payload.outputLimits, {});
  client.dispose();
});

test("generated validators cover result and client error recursively", () => {
  const result = {
    protocolVersion: WORKER_PROTOCOL_VERSION,
    type: "result",
    version: 1,
    generation: 1,
    result: {
      packageVersion: PACKAGE_VERSION,
      version: 1,
      generation: 1,
      products: {
        syntax: false, canonicalAst: false, html: true, attributeOccurrences: false,
        resourceQueries: true, diagnostics: true, symbols: false, projection: true,
      },
      parse: { packageVersion: PACKAGE_VERSION, blockCount: 0, nodeCount: 0, referenceCount: 0 },
      syntax: "", ast: "", html: "", attributeOccurrences: [], resourceQueries: [],
      diagnostics: [], renderDiagnostics: [], symbols: [],
      projection: {
        packageVersion: PACKAGE_VERSION, sourceId: null, sourceBlocks: [], formulas: [],
        blockPresentations: [], orderedLists: [], referenceEdges: [], externalLinks: [],
        searchableText: { text: "", segments: [] },
        structure: { headings: [], toc: [], manpage: null },
        catalogs: { footnotes: [], bibliography: [], index: [] },
        targets: [], title: null,
      },
    },
  };
  assertWorkerContract(result, "responses");
  assert.equal(validateWorkerMessage({
    ...result,
    result: { ...result.result, version: "1" },
  }, "responses"), false);

  const error = { code: "worker-failed", message: "failed", sourceVersion: null, generation: 1 };
  assert.equal(validateClientError(error), true);
  assert.equal(validateClientError({ ...error, generation: "1" }), false);
  assert.equal(validateClientError({ ...error, unexpected: true }), false);
  const missing = { ...error };
  delete missing.code;
  assert.equal(validateClientError(missing), false);
});

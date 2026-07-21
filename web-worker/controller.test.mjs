import assert from "node:assert/strict";
import test from "node:test";

import {
  WORKER_PROTOCOL_VERSION,
  createController,
} from "./controller.mjs";
import { AdocWeaveWorkerClient } from "./client.mjs";
import { WASM_API_VERSION } from "./contracts.mjs";

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
      apiVersion: WASM_API_VERSION,
      sourceId: null,
      version,
      generation,
      source: `version ${version}`,
    },
  };
}

test("debounce publishes only the latest document generation", () => {
  const state = harness();
  Atomics.store(state.cancellation, 0, 1);
  state.controller.submit(request(1, 1));
  Atomics.store(state.cancellation, 0, 2);
  state.controller.submit(request(2, 2));
  state.flush();

  assert.equal(state.messages.length, 1);
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
  assert.equal(state.messages[0].error.code, "unsupported-worker-protocol");
});

test("client sends the current WASM API version with core-owned default options", async () => {
  const messages = [];
  class FakeWorker {
    listeners = new Map();
    postMessage(message) {
      messages.push(message);
      if (message.type === "initialize") {
        queueMicrotask(() => this.listeners.get("message")?.({ data: { type: "ready" } }));
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

  assert.equal(messages[1].payload.apiVersion, WASM_API_VERSION);
  assert.deepEqual(messages[1].payload.options, {});
  client.dispose();
});

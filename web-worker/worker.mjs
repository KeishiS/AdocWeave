import { createController, WORKER_PROTOCOL_VERSION } from "./controller.mjs";

let controller;
let currentGeneration = 0;

self.onmessage = async ({ data }) => {
  if (data?.type === "initialize") {
    if (
      data.protocolVersion !== WORKER_PROTOCOL_VERSION
    ) {
      throw new Error("invalid AdocWeave worker initialization");
    }
    const wasm = await import(data.moduleUrl);
    await wasm.default(data.wasmUrl);
    const cancellation = data.cancellationBuffer === null
      ? null
      : new Int32Array(data.cancellationBuffer);
    controller = createController({
      process: wasm.process,
      publish: (message) => self.postMessage(message),
      isCurrent: (generation) => cancellation === null
        ? currentGeneration === generation
        : Atomics.load(cancellation, 0) === generation,
      debounceMs: data.debounceMs,
    });
    self.postMessage({
      protocolVersion: WORKER_PROTOCOL_VERSION,
      type: "ready",
    });
    return;
  }
  if (data?.type === "analyze") {
    // A fallback worker receives one current request. Its termination is the
    // cancellation boundary, while this value handles debounce consistently.
    currentGeneration = data.generation;
  }
  controller?.submit(data);
};

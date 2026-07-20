import { createController, WORKER_PROTOCOL_VERSION } from "./controller.mjs";

let controller;

self.onmessage = async ({ data }) => {
  if (data?.type === "initialize") {
    if (
      data.protocolVersion !== WORKER_PROTOCOL_VERSION ||
      !(data.cancellationBuffer instanceof SharedArrayBuffer)
    ) {
      throw new Error("invalid AdocWeave worker initialization");
    }
    const wasm = await import(data.moduleUrl);
    await wasm.default(data.wasmUrl);
    controller = createController({
      process: wasm.process,
      publish: (message) => self.postMessage(message),
      cancellation: new Int32Array(data.cancellationBuffer),
      debounceMs: data.debounceMs,
    });
    self.postMessage({
      protocolVersion: WORKER_PROTOCOL_VERSION,
      type: "ready",
    });
    return;
  }
  controller?.submit(data);
};

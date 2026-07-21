import { WORKER_PROTOCOL_VERSION } from "./controller.mjs";

export class AdocWeaveWorkerClient {
  #worker;
  #cancellation = new Int32Array(new SharedArrayBuffer(4));
  #generation = 0;

  constructor(worker, { moduleUrl, wasmUrl, debounceMs = 40 }) {
    this.#worker = worker;
    worker.postMessage({
      protocolVersion: WORKER_PROTOCOL_VERSION,
      type: "initialize",
      moduleUrl,
      wasmUrl,
      debounceMs,
      cancellationBuffer: this.#cancellation.buffer,
    });
  }

  analyze({ sourceId = null, version, source, options = {} }) {
    const generation = ++this.#generation;
    Atomics.store(this.#cancellation, 0, generation);
    this.#worker.postMessage({
      protocolVersion: WORKER_PROTOCOL_VERSION,
      type: "analyze",
      version,
      generation,
      payload: {
        apiVersion: 5,
        sourceId,
        version,
        generation,
        source,
        options,
      },
    });
    return generation;
  }

  cancel() {
    Atomics.store(this.#cancellation, 0, ++this.#generation);
  }

  dispose() {
    this.cancel();
    this.#worker.terminate();
  }
}

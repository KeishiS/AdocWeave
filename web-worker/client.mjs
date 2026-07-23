import { WORKER_PROTOCOL_VERSION } from "./controller.mjs";
import { CONTRACT_VERSION } from "./contracts.mjs";

export class AdocWeaveClient {
  #options;
  #worker = null;
  #cancellation = null;
  #generation = 0;
  #disposed = false;
  #ready = null;

  constructor({
    workerUrl,
    moduleUrl,
    wasmUrl,
    debounceMs = 40,
    onResult = () => {},
    onError = () => {},
    Worker: WorkerConstructor = globalThis.Worker,
    sharedCancellation = globalThis.crossOriginIsolated === true &&
      typeof globalThis.SharedArrayBuffer === "function",
  }) {
    if (typeof WorkerConstructor !== "function") {
      throw new TypeError("a Worker constructor is required");
    }
    this.#options = {
      workerUrl: String(workerUrl),
      moduleUrl: String(moduleUrl),
      wasmUrl: String(wasmUrl),
      debounceMs,
      onResult,
      onError,
      WorkerConstructor,
      sharedCancellation,
    };
    if (sharedCancellation) {
      this.#cancellation = new Int32Array(new SharedArrayBuffer(4));
      this.#spawnWorker();
    }
  }

  update({ sourceId = null, version, source, renderInputs, options = {} }) {
    this.#assertActive();
    const generation = ++this.#generation;
    if (this.#options.sharedCancellation) {
      Atomics.store(this.#cancellation, 0, generation);
    } else {
      // Without SharedArrayBuffer, terminating the previous synchronous WASM
      // execution is the only reliable cancellation mechanism.
      this.#terminateWorker();
      this.#spawnWorker();
    }
    const payload = {
      apiVersion: CONTRACT_VERSION,
      sourceId,
      version,
      generation,
      source,
      options,
    };
    if (renderInputs !== undefined) payload.renderInputs = renderInputs;
    this.#ready.then(() => {
      if (!this.#disposed && generation === this.#generation) {
        this.#worker.postMessage({
          protocolVersion: WORKER_PROTOCOL_VERSION,
          type: "analyze",
          version,
          generation,
          payload,
        });
      }
    }).catch(() => {});
    return generation;
  }

  cancel() {
    this.#assertActive();
    ++this.#generation;
    if (this.#options.sharedCancellation) {
      Atomics.store(this.#cancellation, 0, this.#generation);
    } else {
      this.#terminateWorker();
    }
  }

  dispose() {
    if (this.#disposed) return;
    this.#disposed = true;
    ++this.#generation;
    if (this.#cancellation) Atomics.store(this.#cancellation, 0, this.#generation);
    this.#terminateWorker();
  }

  #spawnWorker() {
    const worker = new this.#options.WorkerConstructor(this.#options.workerUrl, {
      type: "module",
    });
    this.#worker = worker;
    this.#ready = new Promise((resolve, reject) => {
      const onMessage = ({ data }) => {
        if (worker !== this.#worker || this.#disposed) return;
        if (data?.type === "ready") {
          resolve();
        } else if (data?.type === "result" && data.generation === this.#generation) {
          const contractVersion = verifiedContractVersion(data.result);
          if (contractVersion === null) {
            this.#options.onError({
              code: "unsupported-contract-version",
              message: `expected contract version ${CONTRACT_VERSION}`,
              sourceVersion: data.version,
              generation: data.generation,
            });
            return;
          }
          this.#options.onResult({
            html: data.result.html,
            diagnostics: data.result.diagnostics,
            renderDiagnostics: data.result.renderDiagnostics,
            sourceVersion: data.version,
            generation: data.generation,
            contractVersion,
            result: data.result,
          });
        } else if (data?.type === "error" && data.generation === this.#generation) {
          this.#options.onError({ ...data.error, sourceVersion: data.version, generation: data.generation });
        }
      };
      worker.addEventListener("message", onMessage);
      worker.addEventListener("error", (event) => {
        if (worker !== this.#worker || this.#disposed) return;
        const error = new Error(event.message || "AdocWeave worker failed");
        reject(error);
        this.#publishError(error, null, this.#generation);
      }, { once: true });
    });
    this.#ready.catch(() => {});
    worker.postMessage({
      protocolVersion: WORKER_PROTOCOL_VERSION,
      type: "initialize",
      moduleUrl: this.#options.moduleUrl,
      wasmUrl: this.#options.wasmUrl,
      debounceMs: this.#options.debounceMs,
      cancellationBuffer: this.#cancellation?.buffer ?? null,
    });
  }

  #terminateWorker() {
    this.#worker?.terminate();
    this.#worker = null;
    this.#ready = null;
  }

  #publishError(error, sourceVersion, generation) {
    if (generation !== this.#generation || this.#disposed) return;
    this.#options.onError({
      code: "worker-failed",
      message: error instanceof Error ? error.message : String(error),
      sourceVersion,
      generation,
    });
  }

  #assertActive() {
    if (this.#disposed) throw new Error("AdocWeaveClient is disposed");
  }
}

function verifiedContractVersion(result) {
  return result?.apiVersion === CONTRACT_VERSION ? result.apiVersion : null;
}

export { AdocWeaveClient as AdocWeaveWorkerClient };

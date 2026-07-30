import { WORKER_PROTOCOL_VERSION } from "./controller.mjs";
import { PACKAGE_VERSION } from "./contracts.mjs";
import { validateWorkerMessage } from "./protocol.generated.mjs";

export class AdocWeaveClient {
  #options;
  #worker = null;
  #cancellation = null;
  #generation = 0;
  #disposed = false;
  #ready = null;
  #readyReject = null;
  #pending = new Map();
  #expectedVersions = new Map();

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
    }
  }

  get ready() {
    if (this.#disposed) return Promise.reject(this.#clientError(
      "disposed", "AdocWeaveClient was disposed", this.#generation,
    ));
    return this.#ensureWorker();
  }

  analyze(request) {
    return new Promise((resolve, reject) => {
      if (this.#disposed) {
        reject(this.#clientError(
          "disposed", "AdocWeaveClient was disposed", this.#generation,
        ));
        return;
      }
      const generation = ++this.#generation;
      this.#rejectPending("superseded", "analysis was superseded");
      this.#pending.set(generation, { resolve, reject });
      this.#dispatch(request, generation);
    });
  }

  update(request) {
    this.#assertActive();
    const generation = ++this.#generation;
    this.#rejectPending("superseded", "analysis was superseded");
    this.#dispatch(request, generation);
    return generation;
  }

  #dispatch({
    sourceId = null,
    version,
    source,
    preprocess,
    products,
    renderInputs,
    analysisOptions = {},
    renderPolicy = {},
    outputLimits = {},
  }, generation) {
    this.#expectedVersions.clear();
    this.#expectedVersions.set(generation, version);
    let ready;
    if (this.#options.sharedCancellation) {
      Atomics.store(this.#cancellation, 0, generation);
      ready = this.#ensureWorker();
    } else {
      // Without SharedArrayBuffer, terminating the previous synchronous WASM
      // execution is the only reliable cancellation mechanism.
      this.#terminateWorker(new AdocWeaveClientError({
        code: "superseded",
        message: "worker initialization was superseded",
        sourceVersion: null,
        generation,
      }));
      ready = this.#spawnWorker();
    }
    const payload = {
      packageVersion: PACKAGE_VERSION,
      sourceId,
      version,
      generation,
      source,
      analysisOptions,
      renderPolicy,
      outputLimits,
    };
    if (products !== undefined) payload.products = products;
    if (preprocess !== undefined) payload.preprocess = preprocess;
    if (renderInputs !== undefined) payload.renderInputs = renderInputs;
    ready.then(() => {
      if (!this.#disposed && generation === this.#generation) {
        try {
          this.#worker.postMessage({
            protocolVersion: WORKER_PROTOCOL_VERSION,
            type: "analyze",
            version,
            generation,
            payload,
          });
        } catch (cause) {
          this.#failWorker(cause, generation, this.#worker);
        }
      }
    }).catch(() => {});
  }

  cancel() {
    this.#assertActive();
    this.#rejectPending("cancelled", "analysis was cancelled");
    ++this.#generation;
    this.#expectedVersions.clear();
    if (this.#options.sharedCancellation) {
      Atomics.store(this.#cancellation, 0, this.#generation);
    } else {
      this.#terminateWorker(new AdocWeaveClientError({
        code: "cancelled",
        message: "worker initialization was cancelled",
        sourceVersion: null,
        generation: this.#generation,
      }));
    }
  }

  dispose() {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#rejectPending("disposed", "AdocWeaveClient was disposed");
    ++this.#generation;
    this.#expectedVersions.clear();
    if (this.#cancellation) Atomics.store(this.#cancellation, 0, this.#generation);
    this.#terminateWorker(new AdocWeaveClientError({
      code: "disposed",
      message: "AdocWeaveClient was disposed",
      sourceVersion: null,
      generation: this.#generation,
    }));
  }

  #spawnWorker() {
    let worker;
    try {
      if (typeof this.#options.WorkerConstructor !== "function") {
        throw new TypeError("a Worker constructor is required");
      }
      worker = new this.#options.WorkerConstructor(this.#options.workerUrl, {
        type: "module",
      });
    } catch (cause) {
      const failedGeneration = this.#generation;
      const error = this.#workerError(cause, failedGeneration);
      const ready = Promise.reject(error);
      ready.catch(() => {});
      this.#ready = ready;
      this.#rejectPendingError(error);
      this.#expectedVersions.delete(failedGeneration);
      this.#notifyError(error);
      return ready;
    }
    this.#worker = worker;
    const ready = new Promise((resolve, reject) => {
      this.#readyReject = reject;
      let initialized = false;
      const onMessage = ({ data }) => {
        if (worker !== this.#worker || this.#disposed) return;
        if (
          !validateWorkerMessage(data, "responses") ||
          data.protocolVersion !== WORKER_PROTOCOL_VERSION
        ) {
          const protocolMismatch = Number.isSafeInteger(data?.protocolVersion) &&
            data.protocolVersion !== WORKER_PROTOCOL_VERSION;
          const code = protocolMismatch
            ? "unsupported-worker-protocol"
            : "invalid-worker-response";
          const message = protocolMismatch
            ? `expected worker protocol ${WORKER_PROTOCOL_VERSION}`
            : "worker returned a response outside the public protocol";
          const error = {
            code,
            message,
            sourceVersion: null,
            generation: this.#generation,
          };
          this.#rejectPendingError(error);
          this.#expectedVersions.delete(error.generation);
          this.#terminateWorker(
            !initialized ? new AdocWeaveClientError(error) : null,
            worker,
          );
          this.#notifyError(error);
          return;
        }
        if (data?.type === "ready") {
          initialized = true;
          this.#readyReject = null;
          resolve();
        } else if (data?.type === "result" && data.generation === this.#generation) {
          if (
            !resultMatchesEnvelope(data) ||
            !this.#responseMatchesRequest(data)
          ) {
            const error = {
              code: "invalid-worker-response",
              message: "worker result identity does not match its request",
              sourceVersion: data.version,
              generation: data.generation,
            };
            this.#rejectPendingError(error);
            this.#expectedVersions.delete(error.generation);
            this.#terminateWorker(null, worker);
            this.#notifyError(error);
            return;
          }
          const packageVersion = verifiedPackageVersion(data.result);
          if (packageVersion === null) {
            const error = {
              code: "unsupported-package-version",
              message: `expected package version ${PACKAGE_VERSION}`,
              sourceVersion: data.version,
              generation: data.generation,
            };
            this.#rejectPendingError(error);
            this.#notifyError(error);
            this.#expectedVersions.delete(data.generation);
            return;
          }
          const { version: sourceVersion, ...products } = data.result;
          const result = { sourceVersion, ...products };
          this.#expectedVersions.delete(data.generation);
          this.#resolvePending(data.generation, result);
          this.#notifyResult(result);
        } else if (data?.type === "error" && data.generation === this.#generation) {
          if (!this.#responseMatchesRequest(data)) {
            const error = {
              code: "invalid-worker-response",
              message: "worker error identity does not match its request",
              sourceVersion: data.version,
              generation: data.generation,
            };
            this.#rejectPendingError(error);
            this.#expectedVersions.delete(error.generation);
            this.#terminateWorker(null, worker);
            this.#notifyError(error);
            return;
          }
          const error = {
            ...data.error,
            sourceVersion: data.version,
            generation: data.generation,
          };
          this.#expectedVersions.delete(data.generation);
          this.#rejectPendingError(error);
          // A trapped instance cannot be reused: the abort left the linear
          // memory and the allocator in an unknown state. Dropping the worker
          // makes the next request start from a fresh instance.
          if (error.code === "wasm-trapped") this.#terminateWorker(null, worker);
          this.#notifyError(error);
        }
      };
      worker.addEventListener("message", onMessage);
      worker.addEventListener("error", (event) => {
        if (worker !== this.#worker || this.#disposed) return;
        const error = {
          code: "worker-failed",
          message: event.message || "AdocWeave worker failed",
          sourceVersion: null,
          generation: this.#generation,
        };
        this.#rejectPendingError(error);
        this.#expectedVersions.delete(error.generation);
        this.#terminateWorker(new AdocWeaveClientError(error), worker);
        this.#notifyError(error);
      }, { once: true });
    });
    this.#ready = ready;
    this.#ready.catch(() => {});
    try {
      worker.postMessage({
        protocolVersion: WORKER_PROTOCOL_VERSION,
        type: "initialize",
        moduleUrl: this.#options.moduleUrl,
        wasmUrl: this.#options.wasmUrl,
        debounceMs: this.#options.debounceMs,
        cancellationBuffer: this.#cancellation?.buffer ?? null,
      });
    } catch (cause) {
      this.#failWorker(cause, this.#generation, worker);
    }
    return ready;
  }

  #terminateWorker(error = null, worker = this.#worker) {
    if (worker !== this.#worker) {
      worker?.terminate();
      return;
    }
    if (error !== null) this.#readyReject?.(error);
    this.#readyReject = null;
    this.#worker?.terminate();
    this.#worker = null;
    this.#ready = null;
  }

  #ensureWorker() {
    if (this.#worker === null) return this.#spawnWorker();
    return this.#ready;
  }

  #resolvePending(generation, result) {
    const pending = this.#pending.get(generation);
    if (pending === undefined) return;
    this.#pending.delete(generation);
    pending.resolve(result);
  }

  #rejectPending(code, message) {
    for (const [generation, pending] of this.#pending) {
      pending.reject(new AdocWeaveClientError({
        code,
        message,
        sourceVersion: null,
        generation,
      }));
    }
    this.#pending.clear();
  }

  #rejectPendingError(error) {
    const pending = this.#pending.get(error.generation);
    if (pending === undefined) return;
    this.#pending.delete(error.generation);
    pending.reject(
      error instanceof AdocWeaveClientError
        ? error
        : new AdocWeaveClientError(error),
    );
  }

  #assertActive() {
    if (this.#disposed) throw new Error("AdocWeaveClient is disposed");
  }

  #clientError(code, message, generation, sourceVersion = null) {
    return new AdocWeaveClientError({ code, message, sourceVersion, generation });
  }

  #workerError(cause, generation) {
    return this.#clientError(
      "worker-failed",
      cause instanceof Error ? cause.message : String(cause),
      generation,
    );
  }

  #notifyError(error) {
    const notification = {
      code: error.code,
      message: error.message,
      sourceVersion: error.sourceVersion,
      generation: error.generation,
    };
    queueMicrotask(() => {
      try {
        this.#options.onError(notification);
      } catch {
        // Promise settlement and lifecycle cleanup do not depend on callbacks.
      }
    });
  }

  #notifyResult(result) {
    try {
      this.#options.onResult(result);
    } catch {
      // Promise settlement and worker message handling do not depend on callbacks.
    }
  }

  #responseMatchesRequest({ version, generation }) {
    return this.#expectedVersions.get(generation) === version;
  }

  #failWorker(cause, generation, worker) {
    const error = this.#workerError(cause, generation);
    this.#rejectPendingError(error);
    this.#expectedVersions.delete(generation);
    this.#terminateWorker(error, worker);
    this.#notifyError(error);
  }
}

export class AdocWeaveClientError extends Error {
  constructor({ code, message, sourceVersion, generation }) {
    super(message);
    this.name = "AdocWeaveClientError";
    this.code = code;
    this.sourceVersion = sourceVersion;
    this.generation = generation;
  }
}

const LIFECYCLE_ERROR_CODES = new Set([
  "cancelled",
  "disposed",
  "invalid-worker-response",
  "superseded",
  "unsupported-package-version",
  "unsupported-worker-protocol",
  "wasm-trapped",
  "worker-failed",
]);

export function isAdocWeaveClientLifecycleError(error) {
  return error instanceof AdocWeaveClientError &&
    LIFECYCLE_ERROR_CODES.has(error.code);
}

function verifiedPackageVersion(result) {
  return result?.packageVersion === PACKAGE_VERSION ? result.packageVersion : null;
}

function resultMatchesEnvelope({ version, generation, result }) {
  return result.version === version &&
    result.generation === generation;
}

export { AdocWeaveClient as AdocWeaveWorkerClient };

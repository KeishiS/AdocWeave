export { WORKER_PROTOCOL_VERSION } from "./contracts.mjs";
import { WORKER_PROTOCOL_VERSION } from "./contracts.mjs";

export function createController({
  process,
  publish,
  isCurrent,
  debounceMs = 40,
  schedule = setTimeout,
  unschedule = clearTimeout,
}) {
  let pending = null;
  let latestGeneration = 0;

  function cancelPending() {
    if (pending !== null) {
      unschedule(pending);
      pending = null;
    }
  }

  function submit(request) {
    if (request.protocolVersion !== WORKER_PROTOCOL_VERSION) {
      publish({
        protocolVersion: WORKER_PROTOCOL_VERSION,
        type: "error",
        version: request.version,
        generation: request.generation,
        error: {
          code: "unsupported-worker-protocol",
          message: `expected protocol ${WORKER_PROTOCOL_VERSION}`,
        },
      });
      return;
    }
    latestGeneration = Math.max(latestGeneration, request.generation);
    cancelPending();
    pending = schedule(() => {
      pending = null;
      const generation = request.generation;
      if (
        generation !== latestGeneration ||
        !isCurrent(generation)
      ) {
        return;
      }
      try {
        const result = process(request.payload, () => {
          return !isCurrent(generation);
        });
        if (
          generation === latestGeneration &&
          isCurrent(generation)
        ) {
          publish({
            protocolVersion: WORKER_PROTOCOL_VERSION,
            type: "result",
            version: request.version,
            generation,
            result,
          });
        }
      } catch (error) {
        if (isCurrent(generation)) {
          publish({
            protocolVersion: WORKER_PROTOCOL_VERSION,
            type: "error",
            version: request.version,
            generation,
            error: normalizeError(error),
          });
        }
      }
    }, debounceMs);
  }

  return { submit, cancelPending };
}

function normalizeError(error) {
  if (typeof error === "string") {
    try {
      return JSON.parse(error);
    } catch {
      return { code: "worker-failed", message: error };
    }
  }
  const message = error instanceof Error ? error.message : String(error);
  // A Rust panic reaches this point as a WebAssembly trap because the browser
  // profile aborts instead of unwinding. The instance keeps whatever state the
  // panic left behind, so the client has to discard it rather than reuse it.
  if (isWebAssemblyTrap(error)) {
    return { code: "wasm-trapped", message };
  }
  return { code: "worker-failed", message };
}

function isWebAssemblyTrap(error) {
  return typeof WebAssembly !== "undefined" &&
    typeof WebAssembly.RuntimeError === "function" &&
    error instanceof WebAssembly.RuntimeError;
}

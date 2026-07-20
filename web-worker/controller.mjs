export const WORKER_PROTOCOL_VERSION = 1;

export function createController({
  process,
  publish,
  cancellation,
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
        Atomics.load(cancellation, 0) !== generation
      ) {
        return;
      }
      try {
        const result = process(request.payload, () => {
          return Atomics.load(cancellation, 0) !== generation;
        });
        if (
          generation === latestGeneration &&
          Atomics.load(cancellation, 0) === generation
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
        if (Atomics.load(cancellation, 0) === generation) {
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
  return {
    code: "worker-failed",
    message: error instanceof Error ? error.message : String(error),
  };
}

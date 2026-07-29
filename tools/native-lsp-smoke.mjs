import { spawn } from "node:child_process";
import { rm } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { shouldRetryRemoval } from "./platform-contract.mjs";
import { hasExited, waitForExit } from "./process-lifecycle.mjs";

export const LSP_SMOKE_TOTAL_TIMEOUT_MS = 45_000;
export const LSP_SMOKE_TEARDOWN_RESERVE_MS = 10_000;
const PROCESS_STOP_GRACE_MS = 1_000;
const PROCESS_KILL_GRACE_MS = 1_000;
const REMOVAL_RETRY_DELAY_MS = 100;

export function createNativeSmokeDeadline(
  timeoutMs = LSP_SMOKE_TOTAL_TIMEOUT_MS,
  {
    clearTimer = clearTimeout,
    now = Date.now,
    setTimer = setTimeout,
  } = {},
) {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new TypeError("native smoke timeout must be a positive number");
  }
  const controller = new AbortController();
  const startedAt = now();
  const expiresAt = startedAt + timeoutMs;
  const timeoutError = () => new Error(
    `native LSP smoke exceeded its ${timeoutMs} ms total deadline`,
  );
  const timer = setTimer(() => controller.abort(timeoutError()), timeoutMs);

  return Object.freeze({
    signal: controller.signal,
    timeoutMs,
    remainingMs(reserveMs = 0) {
      return Math.max(0, expiresAt - now() - reserveMs);
    },
    async run(operation, phase, reserveMs = 0) {
      controller.signal.throwIfAborted();
      const remainingMs = this.remainingMs(reserveMs);
      if (remainingMs <= 0) {
        throw new Error(
          `native LSP smoke reached its total deadline during ${phase}`,
        );
      }
      let phaseTimer;
      let abort;
      try {
        return await Promise.race([
          Promise.resolve(operation),
          new Promise((_, reject) => {
            phaseTimer = setTimer(() => reject(new Error(
              `native LSP smoke reached its total deadline during ${phase}`,
            )), remainingMs);
          }),
          new Promise((_, reject) => {
            abort = () => reject(controller.signal.reason);
            controller.signal.addEventListener("abort", abort, { once: true });
          }),
        ]);
      } finally {
        clearTimer(phaseTimer);
        controller.signal.removeEventListener("abort", abort);
      }
    },
    dispose() {
      clearTimer(timer);
    },
  });
}

export async function smokeLsp(
  binary,
  packageVersion,
  deadline,
  {
    clearTimer = clearTimeout,
    documentUri = pathToFileURL(resolve("adocweave-smoke.adoc")).href,
    spawnProcess = spawn,
    setTimer = setTimeout,
    waitForProcessExit = waitForExit,
  } = {},
) {
  const teardownReserveMs = Math.min(
    LSP_SMOKE_TEARDOWN_RESERVE_MS,
    deadline.timeoutMs / 2,
  );
  const child = spawnProcess(binary, [], { stdio: ["pipe", "pipe", "pipe"] });
  const lifecycle = observeLifecycle(child);
  const reader = createJsonRpcReader(child, lifecycle);
  let completed = false;
  try {
    await deadline.run(
      lifecycle.spawned,
      "Language Server process startup",
      teardownReserveMs,
    );
    send(child, {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: { processId: null, rootUri: null, capabilities: {} },
    });
    const initialized = await reader.waitFor(
      (message) => message.id === 1,
      deadline,
      "initialize response",
      teardownReserveMs,
    );
    assertSuccessfulResponse(initialized, "initialize");
    if (initialized.result?.serverInfo?.version !== packageVersion) {
      throw new Error("LSP serverInfo version mismatch");
    }

    send(child, { jsonrpc: "2.0", method: "initialized", params: {} });
    send(child, {
      jsonrpc: "2.0",
      method: "textDocument/didOpen",
      params: {
        textDocument: {
          uri: documentUri,
          languageId: "asciidoc",
          version: 1,
          text: "=Bad\n",
        },
      },
    });
    const diagnostics = await reader.waitFor(
      (message) => message.method === "textDocument/publishDiagnostics",
      deadline,
      "publishDiagnostics notification",
      teardownReserveMs,
    );
    if (!Array.isArray(diagnostics.params?.diagnostics) ||
        diagnostics.params.diagnostics.length === 0) {
      throw new Error("LSP smoke fixture produced no diagnostics");
    }

    send(child, { jsonrpc: "2.0", id: 2, method: "shutdown", params: null });
    const shutdown = await reader.waitFor(
      (message) => message.id === 2,
      deadline,
      "shutdown response",
      teardownReserveMs,
    );
    assertSuccessfulResponse(shutdown, "shutdown");
    send(child, { jsonrpc: "2.0", method: "exit", params: null });
    child.stdin.end();
    await deadline.run(
      lifecycle.closed,
      "Language Server process exit",
      teardownReserveMs / 2,
    );
    if (lifecycle.exitCode !== 0) {
      throw new Error(
        `LSP exited with ${formatExit(lifecycle.exitCode, lifecycle.signalCode)}`,
      );
    }
    completed = true;
  } finally {
    reader.stop();
    if (!completed || !lifecycle.closedState) {
      await terminateProcess(
        child,
        lifecycle,
        deadline,
        waitForProcessExit,
        { clearTimer, setTimer },
      );
    }
    destroyProcessStreams(child);
    lifecycle.stop();
  }
}

export async function removeNativeSmokeDirectory(
  directory,
  deadline,
  {
    delay = abortableDelay,
    platform = process.platform,
    removeDirectory = (path) => rm(path, { recursive: true, force: true }),
  } = {},
) {
  let attempts = 0;
  let lastRetryableError;
  while (true) {
    attempts += 1;
    try {
      await deadline.run(
        removeDirectory(directory),
        "temporary directory cleanup",
      );
      return attempts;
    } catch (error) {
      if (!shouldRetryRemoval(error, platform)) {
        if (deadline.signal.aborted && lastRetryableError) {
          throw cleanupDeadlineError(attempts, lastRetryableError, error);
        }
        throw error;
      }
      lastRetryableError = error;
      const remainingMs = deadline.remainingMs();
      if (remainingMs <= REMOVAL_RETRY_DELAY_MS) {
        throw cleanupDeadlineError(attempts, error);
      }
      try {
        await deadline.run(
          delay(Math.min(REMOVAL_RETRY_DELAY_MS, remainingMs), deadline.signal),
          "temporary directory cleanup retry",
        );
      } catch (deadlineError) {
        throw cleanupDeadlineError(attempts, error, deadlineError);
      }
    }
  }
}

export function combineNativeSmokeErrors(operationError, cleanupError) {
  const cleanup = asError(cleanupError);
  if (!operationError) return cleanup;
  const operation = asError(operationError);
  return new AggregateError(
    [operation, cleanup],
    `${operation.message}\n一時ディレクトリの削除にも失敗しました: ${cleanup.message}`,
    { cause: operation },
  );
}

function createJsonRpcReader(child, lifecycle) {
  let buffer = Buffer.alloc(0);
  let stopped = false;
  const messages = [];
  let notifyChanged;
  let changed = new Promise((resolve) => { notifyChanged = resolve; });
  let readerFailure;
  let rejectReaderFailure;
  const failed = new Promise((_, reject) => { rejectReaderFailure = reject; });
  failed.catch(() => {});

  const fail = (error) => {
    if (readerFailure || stopped) return;
    readerFailure = error instanceof Error ? error : new Error(String(error));
    rejectReaderFailure(readerFailure);
    notifyChanged();
  };
  const receive = (chunk) => {
    try {
      buffer = Buffer.concat([buffer, chunk]);
      while (true) {
        const boundary = buffer.indexOf("\r\n\r\n");
        if (boundary < 0) return;
        const header = buffer.subarray(0, boundary).toString("ascii");
        const match = /(?:^|\r\n)Content-Length: (\d+)(?:\r\n|$)/i.exec(header);
        if (!match) throw new Error("LSP response has no Content-Length");
        const length = Number(match[1]);
        const end = boundary + 4 + length;
        if (buffer.length < end) return;
        messages.push(JSON.parse(
          buffer.subarray(boundary + 4, end).toString("utf8"),
        ));
        buffer = buffer.subarray(end);
        notifyChanged();
        changed = new Promise((resolve) => { notifyChanged = resolve; });
      }
    } catch (error) {
      fail(error);
    }
  };
  const stdoutError = (error) => fail(
    new Error(`failed to read LSP stdout: ${error.message}`, { cause: error }),
  );
  const stdinError = (error) => fail(
    new Error(`failed to write LSP stdin: ${error.message}`, { cause: error }),
  );
  const processError = (error) => fail(
    new Error(`failed to start LSP process: ${error.message}`, { cause: error }),
  );
  const prematureExit = () => {
    if (!stopped) {
      fail(new Error(
        `LSP exited before completing JSON-RPC: ${formatExit(
          lifecycle.exitCode,
          lifecycle.signalCode,
        )}${lifecycle.stderr ? `\n${lifecycle.stderr}` : ""}`,
      ));
    }
  };
  child.stdout.on("data", receive);
  child.stdout.once("error", stdoutError);
  child.stdin.once("error", stdinError);
  child.once("error", processError);
  child.once("exit", prematureExit);

  return {
    async waitFor(predicate, deadline, phase, teardownReserveMs) {
      while (true) {
        if (readerFailure) throw readerFailure;
        const found = messages.find(predicate);
        if (found) {
          messages.splice(messages.indexOf(found), 1);
          return found;
        }
        await deadline.run(
          Promise.race([
            changed,
            failed,
            lifecycle.exited.then(() => {
              throw readerFailure ?? new Error(
                `LSP exited while waiting for ${phase}: ${formatExit(
                  lifecycle.exitCode,
                  lifecycle.signalCode,
                )}${lifecycle.stderr ? `\n${lifecycle.stderr}` : ""}`,
              );
            }),
          ]),
          phase,
          teardownReserveMs,
        );
      }
    },
    stop() {
      stopped = true;
      child.stdout.off("data", receive);
      child.stdout.off("error", stdoutError);
      child.stdin.off("error", stdinError);
      child.off("error", processError);
      child.off("exit", prematureExit);
    },
  };
}

function observeLifecycle(child) {
  let spawnedState = false;
  let exitedState = hasExited(child);
  let closedState = false;
  let exitCode = child.exitCode;
  let signalCode = child.signalCode;
  let stderr = "";
  let resolveSpawned;
  let rejectSpawned;
  let resolveExited;
  let resolveClosed;
  const spawned = new Promise((resolve, reject) => {
    resolveSpawned = resolve;
    rejectSpawned = reject;
  });
  const exited = new Promise((resolve) => { resolveExited = resolve; });
  const closed = new Promise((resolve) => { resolveClosed = resolve; });
  const onSpawn = () => {
    spawnedState = true;
    resolveSpawned();
  };
  const onError = (error) => {
    if (!spawnedState) rejectSpawned(
      new Error(`failed to start LSP process: ${error.message}`, { cause: error }),
    );
  };
  const onExit = (code, signal) => {
    exitedState = true;
    exitCode = code;
    signalCode = signal;
    resolveExited();
  };
  const onClose = (code, signal) => {
    closedState = true;
    exitCode = code;
    signalCode = signal;
    resolveExited();
    resolveClosed();
  };
  const onStderr = (chunk) => {
    stderr = `${stderr}${chunk}`.slice(-8192);
  };
  child.once("spawn", onSpawn);
  child.once("error", onError);
  child.once("exit", onExit);
  child.once("close", onClose);
  child.stderr?.on("data", onStderr);
  if (exitedState) {
    resolveExited();
    resolveClosed();
  }
  return {
    spawned,
    exited,
    closed,
    get closedState() { return closedState; },
    get exitCode() { return exitCode; },
    get signalCode() { return signalCode; },
    get stderr() { return stderr; },
    stop() {
      child.off("spawn", onSpawn);
      child.off("error", onError);
      child.off("exit", onExit);
      child.off("close", onClose);
      child.stderr?.off("data", onStderr);
    },
  };
}

async function terminateProcess(
  child,
  lifecycle,
  deadline,
  waitForProcessExit,
  timers,
) {
  child.stdin?.destroy?.();
  if (hasExited(child)) {
    await waitForCloseWithinDeadline(lifecycle, deadline, timers);
    return;
  }
  child.kill();
  if (await waitWithinDeadline(
    (signal) => waitForProcessExit(
      child,
      PROCESS_STOP_GRACE_MS,
      { signal },
    ),
    deadline,
    "Language Server graceful termination",
  )) {
    await waitForCloseWithinDeadline(lifecycle, deadline, timers);
    return;
  }
  child.kill("SIGKILL");
  if (await waitWithinDeadline(
    (signal) => waitForProcessExit(
      child,
      PROCESS_KILL_GRACE_MS,
      { signal },
    ),
    deadline,
    "Language Server forced termination",
  )) {
    await waitForCloseWithinDeadline(lifecycle, deadline, timers);
    return;
  }
  child.stdout?.destroy?.();
  child.stderr?.destroy?.();
  child.unref?.();
}

async function waitForCloseWithinDeadline(
  lifecycle,
  deadline,
  {
    clearTimer,
    setTimer,
  },
) {
  if (lifecycle.closedState) return;
  let timer;
  try {
    await deadline.run(
      Promise.race([
        lifecycle.closed,
        new Promise((resolve) => {
          timer = setTimer(resolve, 500);
        }),
      ]),
      "Language Server stdio closure",
    );
  } catch {
    // The process has exited; stream destruction below releases remaining handles.
  } finally {
    clearTimer(timer);
  }
}

async function waitWithinDeadline(operation, deadline, phase) {
  if (deadline.remainingMs() <= 0) return false;
  const controller = new AbortController();
  const abort = () => controller.abort(deadline.signal.reason);
  deadline.signal.addEventListener("abort", abort, { once: true });
  try {
    return await deadline.run(operation(controller.signal), phase);
  } catch {
    return false;
  } finally {
    deadline.signal.removeEventListener("abort", abort);
    if (!controller.signal.aborted) {
      controller.abort(new Error(`${phase} wait ended`));
    }
  }
}

function cleanupDeadlineError(attempts, removalError, deadlineError) {
  const detail = deadlineError ? `: ${deadlineError.message}` : "";
  return new Error(
    `native smoke temporary directory cleanup exhausted its total deadline after ${attempts} attempts${detail}`,
    { cause: removalError },
  );
}

function asError(error) {
  return error instanceof Error ? error : new Error(String(error));
}

function destroyProcessStreams(child) {
  child.stdin?.destroy?.();
  child.stdout?.destroy?.();
  child.stderr?.destroy?.();
}

function send(child, message) {
  const body = JSON.stringify(message);
  child.stdin.write(
    `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`,
  );
}

function assertSuccessfulResponse(message, method) {
  if (message.error) {
    throw new Error(
      `LSP ${method} failed: ${message.error.message ?? JSON.stringify(message.error)}`,
    );
  }
}

function formatExit(code, signal) {
  if (code !== null && code !== undefined) return `exit code ${code}`;
  if (signal) return `signal ${signal}`;
  return "an unknown status";
}

function abortableDelay(milliseconds, signal) {
  signal?.throwIfAborted();
  return new Promise((resolve, reject) => {
    const finish = (complete, value) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", aborted);
      complete(value);
    };
    const aborted = () => finish(reject, signal.reason);
    const timer = setTimeout(() => finish(resolve), milliseconds);
    signal?.addEventListener("abort", aborted, { once: true });
  });
}

import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import {
  LSP_SMOKE_TEARDOWN_RESERVE_MS,
  LSP_SMOKE_TOTAL_TIMEOUT_MS,
  combineNativeSmokeErrors,
  createNativeSmokeDeadline,
  removeNativeSmokeDirectory,
  smokeLsp,
} from "./native-lsp-smoke.mjs";

const TEST_PACKAGE_VERSION = "test-package-version";

test("native LSP smoke has an explicit total deadline and teardown reserve", () => {
  assert.equal(LSP_SMOKE_TOTAL_TIMEOUT_MS, 45_000);
  assert.equal(LSP_SMOKE_TEARDOWN_RESERVE_MS, 10_000);
  assert.ok(
    LSP_SMOKE_TOTAL_TIMEOUT_MS - LSP_SMOKE_TEARDOWN_RESERVE_MS > 10_000,
  );
});

test("a stalled JSON-RPC reader reaches the total deadline and releases the process", async () => {
  const child = new FakeChild();
  const deadline = createNativeSmokeDeadline(40);
  const started = Date.now();
  const error = await smokeLsp("adocweave-lsp", TEST_PACKAGE_VERSION, deadline, {
    spawnProcess: () => child,
  }).then(
    () => undefined,
    (failure) => failure,
  );
  deadline.dispose();

  assert.match(error.message, /total deadline during initialize response/);
  assert.ok(Date.now() - started < 1000);
  assert.deepEqual(child.kills, ["SIGTERM"]);
  assert.equal(child.stdin.destroyCount, 2);
  assert.equal(child.stdout.destroyCount, 1);
  assert.equal(child.stderr.destroyCount, 1);
  assert.equal(child.stdout.listenerCount("data"), 0);
});

test("a stubborn process leaves no exit wait after the total deadline", async () => {
  const child = new FakeChild({ stubborn: true });
  const deadline = createNativeSmokeDeadline(40);
  let activeWaits = 0;
  let waitCalls = 0;
  const waitSignals = [];
  const waitForProcessExit = (_child, _milliseconds, { signal } = {}) => {
    waitCalls += 1;
    waitSignals.push(signal);
    assert.notEqual(signal, deadline.signal);
    signal.throwIfAborted();
    activeWaits += 1;
    return new Promise((resolve, reject) => {
      let settled = false;
      const finish = (complete, value) => {
        if (settled) return;
        settled = true;
        signal.removeEventListener("abort", aborted);
        activeWaits -= 1;
        complete(value);
      };
      const aborted = () => finish(reject, signal.reason);
      signal.addEventListener("abort", aborted, { once: true });
      if (signal.aborted) aborted();
    });
  };

  const error = await smokeLsp(
    "adocweave-lsp",
    TEST_PACKAGE_VERSION,
    deadline,
    {
      spawnProcess: () => child,
      waitForProcessExit,
    },
  ).then(
    () => undefined,
    (failure) => failure,
  );
  deadline.dispose();

  assert.match(error.message, /total deadline/);
  assert.deepEqual(child.kills, ["SIGTERM", "SIGKILL"]);
  assert.ok(waitCalls <= 2, waitCalls);
  assert.ok(waitSignals.every((signal) => signal.aborted));
  assert.equal(activeWaits, 0);
  assert.equal(child.listenerCount("exit"), 0);
});

test("process close clears its fallback timer", async () => {
  const child = new FakeChild({
    closeLater: true,
    startupError: new Error("bad executable"),
  });
  const deadline = createNativeSmokeDeadline(1000);
  const timers = new Set();
  let timerCalls = 0;
  const setTimer = (callback, milliseconds) => {
    timerCalls += 1;
    const timer = setTimeout(() => {
      timers.delete(timer);
      callback();
    }, milliseconds);
    timers.add(timer);
    return timer;
  };
  const clearTimer = (timer) => {
    clearTimeout(timer);
    timers.delete(timer);
  };

  await assert.rejects(
    smokeLsp("broken-lsp", TEST_PACKAGE_VERSION, deadline, {
      clearTimer,
      setTimer,
      spawnProcess: () => child,
    }),
    /failed to start LSP process/,
  );
  deadline.dispose();

  assert.equal(timerCalls, 1);
  assert.equal(timers.size, 0);
});

test("a process startup error fails immediately without waiting for the deadline", async () => {
  const child = new FakeChild({ startupError: new Error("bad executable") });
  const deadline = createNativeSmokeDeadline(10_000);
  const started = Date.now();
  const error = await smokeLsp("broken-lsp", TEST_PACKAGE_VERSION, deadline, {
    spawnProcess: () => child,
  }).then(
    () => undefined,
    (failure) => failure,
  );
  deadline.dispose();

  assert.match(error.message, /failed to start LSP process: bad executable/);
  assert.ok(Date.now() - started < 1000);
  assert.deepEqual(child.kills, ["SIGTERM"]);
});

test("a malformed artifact response fails immediately instead of looking like a timeout", async () => {
  const child = new FakeChild({
    onRequest(request, process) {
      if (request.method === "initialize") {
        queueMicrotask(() => process.stdout.emit(
          "data",
          Buffer.from("Content-Length: 1\r\n\r\n{"),
        ));
      }
    },
  });
  const deadline = createNativeSmokeDeadline(10_000);
  const started = Date.now();
  const error = await smokeLsp("adocweave-lsp", TEST_PACKAGE_VERSION, deadline, {
    spawnProcess: () => child,
  }).then(
    () => undefined,
    (failure) => failure,
  );
  deadline.dispose();

  assert.match(error.message, /JSON|property name/);
  assert.ok(Date.now() - started < 1000);
});

test("an LSP error response fails immediately instead of looking like a timeout", async () => {
  const child = new FakeChild({
    onRequest(request, process) {
      if (request.method === "initialize") {
        process.respond({
          jsonrpc: "2.0",
          id: request.id,
          error: { code: -32603, message: "artifact initialization failed" },
        });
      }
    },
  });
  const deadline = createNativeSmokeDeadline(10_000);
  const started = Date.now();
  const error = await smokeLsp(
    "adocweave-lsp",
    TEST_PACKAGE_VERSION,
    deadline,
    { spawnProcess: () => child },
  ).then(
    () => undefined,
    (failure) => failure,
  );
  deadline.dispose();

  assert.match(error.message, /LSP initialize failed: artifact initialization failed/);
  assert.ok(Date.now() - started < 1000);
});

test("a valid JSON-RPC exchange shuts down cleanly", async () => {
  const child = respondingChild();
  const deadline = createNativeSmokeDeadline(1000);
  await smokeLsp("adocweave-lsp", TEST_PACKAGE_VERSION, deadline, {
    spawnProcess: () => child,
  });
  deadline.dispose();

  assert.deepEqual(child.kills, []);
  assert.equal(child.exitCode, 0);
  assert.equal(child.stdout.listenerCount("data"), 0);
  assert.equal(child.stdin.destroyCount, 1);
});

test("the native smoke uses the platform-specific fixture URI", async () => {
  const documentUri = "file:///D:/native%20smoke/fixture.adoc";
  let openedUri;
  const child = respondingChild({
    onDidOpen(request) {
      openedUri = request.params.textDocument.uri;
    },
  });
  const deadline = createNativeSmokeDeadline(1000);
  await smokeLsp("adocweave-lsp", TEST_PACKAGE_VERSION, deadline, {
    documentUri,
    spawnProcess: () => child,
  });
  deadline.dispose();

  assert.equal(openedUri, documentUri);
});

test("Windows EPERM cleanup is retried inside the total deadline", async () => {
  const attempts = [];
  const deadline = createNativeSmokeDeadline(1000);
  const count = await removeNativeSmokeDirectory("temporary", deadline, {
    platform: "win32",
    delay: async () => {},
    removeDirectory: async () => {
      attempts.push("remove");
      if (attempts.length < 3) {
        const error = new Error("locked");
        error.code = "EPERM";
        throw error;
      }
    },
  });
  deadline.dispose();

  assert.equal(count, 3);
  assert.deepEqual(attempts, ["remove", "remove", "remove"]);
});

test("a stalled temporary directory cleanup is bounded by the same deadline", async () => {
  const deadline = createNativeSmokeDeadline(20);
  const started = Date.now();
  const error = await removeNativeSmokeDirectory("temporary", deadline, {
    platform: "win32",
    removeDirectory: async () => new Promise(() => {}),
  }).then(
    () => undefined,
    (failure) => failure,
  );
  deadline.dispose();

  assert.match(error.message, /total deadline/);
  assert.ok(Date.now() - started < 1000);
});

test("Windows cleanup deadline preserves the last EPERM as its cause", async () => {
  const locked = new Error("still locked");
  locked.code = "EPERM";
  const deadline = createNativeSmokeDeadline(20);
  const error = await removeNativeSmokeDirectory("temporary", deadline, {
    platform: "win32",
    removeDirectory: async () => {
      throw locked;
    },
  }).then(
    () => undefined,
    (failure) => failure,
  );
  deadline.dispose();

  assert.match(error.message, /cleanup exhausted its total deadline/);
  assert.equal(error.cause, locked);
});

test("operation and cleanup errors are both retained", () => {
  const operation = new Error("artifact smoke failed");
  const cleanup = new Error("temporary directory remained");
  const combined = combineNativeSmokeErrors(operation, cleanup);

  assert.ok(combined instanceof AggregateError);
  assert.deepEqual(combined.errors, [operation, cleanup]);
  assert.equal(combined.cause, operation);
  assert.match(combined.message, /artifact smoke failed/);
  assert.match(combined.message, /temporary directory remained/);
});

function respondingChild({ onDidOpen } = {}) {
  return new FakeChild({
    onRequest(request, child) {
      if (request.method === "initialize") {
        child.respond({
          jsonrpc: "2.0",
          id: request.id,
          result: { serverInfo: { version: TEST_PACKAGE_VERSION } },
        });
      } else if (request.method === "textDocument/didOpen") {
        onDidOpen?.(request);
        child.respond({
          jsonrpc: "2.0",
          method: "textDocument/publishDiagnostics",
          params: { diagnostics: [{ message: "bad heading" }] },
        });
      } else if (request.method === "shutdown") {
        child.respond({ jsonrpc: "2.0", id: request.id, result: null });
      } else if (request.method === "exit") {
        queueMicrotask(() => child.finish(0, null));
      }
    },
  });
}

class FakeStream extends EventEmitter {
  destroyCount = 0;

  destroy() {
    this.destroyCount += 1;
  }
}

class FakeStdin extends FakeStream {
  constructor(child, onRequest) {
    super();
    this.child = child;
    this.onRequest = onRequest;
  }

  write(payload) {
    const boundary = payload.indexOf("\r\n\r\n");
    const request = JSON.parse(payload.slice(boundary + 4));
    this.onRequest?.(request, this.child);
    return true;
  }

  end() {}
}

class FakeChild extends EventEmitter {
  constructor({ closeLater = false, onRequest, startupError, stubborn = false } = {}) {
    super();
    this.closeLater = closeLater;
    this.exitCode = null;
    this.signalCode = null;
    this.kills = [];
    this.stdout = new FakeStream();
    this.stderr = new FakeStream();
    this.stdin = new FakeStdin(this, onRequest);
    this.stubborn = stubborn;
    queueMicrotask(() => {
      if (startupError) this.emit("error", startupError);
      else this.emit("spawn");
    });
  }

  kill(signal = "SIGTERM") {
    this.kills.push(signal);
    if (this.stubborn) return true;
    queueMicrotask(() => this.finish(null, signal));
    return true;
  }

  finish(code, signal) {
    if (this.exitCode !== null || this.signalCode !== null) return;
    this.exitCode = code;
    this.signalCode = signal;
    this.emit("exit", code, signal);
    if (this.closeLater) {
      setImmediate(() => this.emit("close", code, signal));
    } else {
      this.emit("close", code, signal);
    }
  }

  respond(message) {
    const body = JSON.stringify(message);
    const frame = Buffer.from(
      `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`,
    );
    queueMicrotask(() => this.stdout.emit("data", frame));
  }
}

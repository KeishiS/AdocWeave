import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { hasExited, waitForExit } from "./process-lifecycle.mjs";

function childState() {
  const child = new EventEmitter();
  child.exitCode = null;
  child.signalCode = null;
  return child;
}

test("signal termination is a completed child process state", async () => {
  const child = childState();
  child.signalCode = "SIGTERM";
  assert.equal(hasExited(child), true);
  assert.equal(await waitForExit(child, 100), true);
  assert.equal(child.listenerCount("exit"), 0);
});

test("exit event completes the wait and clears its timer", async () => {
  const child = childState();
  const waiting = waitForExit(child, 100);
  child.exitCode = 0;
  child.emit("exit", 0, null);
  assert.equal(await waiting, true);
});

test("timeout removes its exit listener", async () => {
  const child = childState();
  assert.equal(await waitForExit(child, 1), false);
  assert.equal(child.listenerCount("exit"), 0);
});

test("abort removes the exit listener without waiting for the timeout", async () => {
  const child = childState();
  const controller = new AbortController();
  const reason = new Error("startup deadline");
  const waiting = waitForExit(child, 60_000, { signal: controller.signal });

  controller.abort(reason);

  await assert.rejects(waiting, (error) => error === reason);
  assert.equal(child.listenerCount("exit"), 0);
});

export function hasExited(child) {
  return child.exitCode !== null || child.signalCode !== null;
}

export function waitForExit(child, milliseconds, { signal } = {}) {
  if (hasExited(child)) return Promise.resolve(true);
  signal?.throwIfAborted();
  return new Promise((resolveWait, rejectWait) => {
    let settled = false;
    const finish = (complete, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.off("exit", exited);
      signal?.removeEventListener("abort", aborted);
      complete(value);
    };
    const exited = () => {
      finish(resolveWait, true);
    };
    const aborted = () => {
      finish(rejectWait, signal.reason);
    };
    const timer = setTimeout(() => {
      finish(resolveWait, false);
    }, milliseconds);
    child.once("exit", exited);
    signal?.addEventListener("abort", aborted, { once: true });
    if (signal?.aborted) aborted();
  });
}

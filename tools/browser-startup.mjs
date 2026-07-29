export const BROWSER_STARTUP_ATTEMPTS = 3;
export const BROWSER_STARTUP_ATTEMPT_TIMEOUT_MS = 20_000;
export const BROWSER_STARTUP_TOTAL_TIMEOUT_MS = 75_000;

export async function retryBrowserStartup(
  operation,
  {
    attempts,
    totalTimeoutMs,
    onFailure = () => {},
    now = Date.now,
  },
) {
  if (!Number.isInteger(attempts) || attempts < 1) {
    throw new Error("browser startup attempts must be a positive integer");
  }
  if (!Number.isFinite(totalTimeoutMs) || totalTimeoutMs <= 0) {
    throw new Error("browser startup total timeout must be positive");
  }
  const startedAt = now();
  const deadline = startedAt + totalTimeoutMs;
  let lastError;
  const failures = [];
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const remainingMs = deadline - now();
    if (remainingMs <= 0) break;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      const error = new Error(
        `Chromium startup exceeded the ${totalTimeoutMs} ms total timeout`,
      );
      error.retryBrowserStartup = true;
      controller.abort(error);
    }, remainingMs);
    try {
      return await operation({ attempt, remainingMs, signal: controller.signal });
    } catch (error) {
      lastError = error;
      if (!error.retryBrowserStartup) throw error;
      const currentTime = now();
      const willRetry = attempt < attempts && currentTime < deadline;
      failures.push({ attempt, error });
      onFailure({
        attempt,
        attempts,
        elapsedMs: currentTime - startedAt,
        error,
        willRetry,
      });
      if (!willRetry) break;
    } finally {
      clearTimeout(timer);
    }
  }
  const diagnostics = failures
    .map(({ attempt, error }) => `attempt ${attempt}: ${error.message}`)
    .join("; ");
  throw new Error(
    `Chromium startup exhausted ${failures.length || attempts}/${attempts} attempts within ${totalTimeoutMs} ms: ${diagnostics || lastError?.message || "total timeout"}`,
  );
}

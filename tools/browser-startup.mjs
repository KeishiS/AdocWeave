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
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const remainingMs = deadline - now();
    if (remainingMs <= 0) break;
    try {
      return await operation({ attempt, remainingMs });
    } catch (error) {
      lastError = error;
      if (!error.retryBrowserStartup) throw error;
      const currentTime = now();
      const willRetry = attempt < attempts && currentTime < deadline;
      onFailure({
        attempt,
        attempts,
        elapsedMs: currentTime - startedAt,
        error,
        willRetry,
      });
      if (!willRetry) throw error;
    }
  }
  throw new Error(
    `Chromium startup exhausted ${attempts} attempts within ${totalTimeoutMs} ms: ${lastError?.message ?? "total timeout"}`,
  );
}

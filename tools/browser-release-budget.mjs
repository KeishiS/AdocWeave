export const MAX_BROWSER_ARCHIVE_BYTES = 2 * 1024 * 1024;
export const MAX_BROWSER_WASM_BYTES = 1280 * 1024;

export function browserArtifactSizeError(archiveBytes, wasmBytes) {
  if (archiveBytes > MAX_BROWSER_ARCHIVE_BYTES) {
    return `archive exceeds 2 MiB: ${archiveBytes}`;
  }
  if (wasmBytes > MAX_BROWSER_WASM_BYTES) {
    return `WASM exceeds 1.25 MiB: ${wasmBytes}`;
  }
  return null;
}

export function assertBrowserArtifactSizes(archiveBytes, wasmBytes) {
  const error = browserArtifactSizeError(archiveBytes, wasmBytes);
  if (error !== null) throw new Error(error);
}

export function exitAfterSuccessfulCleanup() {
  // A retried VS Code download can leave helper handles alive after every
  // installation check and cleanup has completed. Do not let those unrelated
  // handles keep the release gate running until its job timeout.
  process.exit(0);
}

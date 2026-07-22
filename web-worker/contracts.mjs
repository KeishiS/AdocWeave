// Keep this thin runtime constant synchronized with release-manifest.json.
// The synchronization test fails whenever the public WASM contract changes.
export const WASM_API_VERSION = 2;
export const CORE_API_VERSION = 2;
export const CORE_PROFILE_VERSION = 1;
export const HTML_CONTRACT_VERSION = 2;
export const PROJECTION_CONTRACT_VERSION = 2;
export const CONFORMANCE_CONTRACT_VERSION = 2;
export const BROWSER_PACKAGE_VERSION = "0.1.0-rc.3";
export const CONTRACT_VERSIONS = Object.freeze({
  conformance: CONFORMANCE_CONTRACT_VERSION,
  coreApi: CORE_API_VERSION,
  coreProfile: CORE_PROFILE_VERSION,
  html: HTML_CONTRACT_VERSION,
  projection: PROJECTION_CONTRACT_VERSION,
  wasmApi: WASM_API_VERSION,
});

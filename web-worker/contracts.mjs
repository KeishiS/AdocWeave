// Keep this thin runtime constant synchronized with release-manifest.json.
// The synchronization test fails whenever the public WASM contract changes.
export const WASM_API_VERSION = 10;
export const CORE_API_VERSION = 11;
export const CORE_PROFILE_VERSION = 7;
export const HTML_CONTRACT_VERSION = 8;
export const PROJECTION_CONTRACT_VERSION = 10;
export const CONFORMANCE_CONTRACT_VERSION = 11;
export const BROWSER_PACKAGE_VERSION = "0.1.0";
export const CONTRACT_VERSIONS = Object.freeze({
  conformance: CONFORMANCE_CONTRACT_VERSION,
  coreApi: CORE_API_VERSION,
  coreProfile: CORE_PROFILE_VERSION,
  html: HTML_CONTRACT_VERSION,
  projection: PROJECTION_CONTRACT_VERSION,
  wasmApi: WASM_API_VERSION,
});

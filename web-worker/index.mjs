export { AdocWeaveClient, AdocWeaveWorkerClient } from "./client.mjs";
export {
  BROWSER_PACKAGE_VERSION,
  CONFORMANCE_CONTRACT_VERSION,
  CONTRACT_VERSIONS,
  CORE_API_VERSION,
  CORE_PROFILE_VERSION,
  HTML_CONTRACT_VERSION,
  PROJECTION_CONTRACT_VERSION,
  WASM_API_VERSION,
} from "./contracts.mjs";

export function defaultAssetUrls(baseUrl = import.meta.url) {
  return {
    workerUrl: new URL("./worker.mjs", baseUrl),
    moduleUrl: new URL("../wasm/adocweave_wasm.js", baseUrl),
    wasmUrl: new URL("../wasm/adocweave_wasm_bg.wasm", baseUrl),
  };
}

export { AdocWeaveClient, AdocWeaveWorkerClient } from "./client.mjs";
export {
  BROWSER_PACKAGE_VERSION,
  CONTRACT_VERSION,
} from "./contracts.mjs";

export function defaultAssetUrls(baseUrl = import.meta.url) {
  return {
    workerUrl: new URL("./worker.mjs", baseUrl),
    moduleUrl: new URL("../wasm/adocweave_wasm.js", baseUrl),
    wasmUrl: new URL("../wasm/adocweave_wasm_bg.wasm", baseUrl),
  };
}

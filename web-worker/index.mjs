import {
  AdocWeaveClient,
  AdocWeaveClientError,
  AdocWeaveWorkerClient,
  isAdocWeaveClientLifecycleError,
} from "./client.mjs";
export {
  AdocWeaveClient,
  AdocWeaveClientError,
  AdocWeaveWorkerClient,
  isAdocWeaveClientLifecycleError,
};
export {
  BROWSER_PACKAGE_VERSION,
  PACKAGE_VERSION,
} from "./contracts.mjs";
export { PROTOCOL_SCHEMA_VERSION } from "./protocol.generated.mjs";

export function defaultAssetUrls(baseUrl = import.meta.url) {
  return {
    workerUrl: new URL("./worker.mjs", baseUrl),
    moduleUrl: new URL("../wasm/adocweave_wasm.js", baseUrl),
    wasmUrl: new URL("../wasm/adocweave_wasm_bg.wasm", baseUrl),
  };
}

export async function analyzeOnce(clientOptions, request) {
  const client = new AdocWeaveClient(clientOptions);
  try {
    await client.ready;
    return await client.analyze(request);
  } finally {
    client.dispose();
  }
}

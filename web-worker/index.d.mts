export interface AdocWeaveContracts {
  conformance: number;
  coreApi: number;
  coreProfile: number;
  html: number;
  projection: number;
  wasmApi: number;
}

export interface AdocWeaveResult {
  html: string;
  diagnostics: unknown[];
  renderDiagnostics: unknown[];
  sourceVersion: number;
  generation: number;
  contracts: AdocWeaveContracts;
  result: unknown;
}

export interface AdocWeaveError {
  code: string;
  message: string;
  sourceVersion: number | null;
  generation: number;
}

export interface UpdateRequest {
  sourceId?: string | null;
  version: number;
  source: string;
  renderInputs?: unknown;
  options?: Record<string, unknown>;
}

export interface AdocWeaveClientOptions {
  workerUrl: string | URL;
  moduleUrl: string | URL;
  wasmUrl: string | URL;
  debounceMs?: number;
  onResult?: (result: AdocWeaveResult) => void;
  onError?: (error: AdocWeaveError) => void;
  Worker?: typeof Worker;
  sharedCancellation?: boolean;
}

export declare class AdocWeaveClient {
  constructor(options: AdocWeaveClientOptions);
  update(request: UpdateRequest): number;
  cancel(): void;
  dispose(): void;
}

export { AdocWeaveClient as AdocWeaveWorkerClient };
export declare function defaultAssetUrls(baseUrl?: string | URL): {
  workerUrl: URL;
  moduleUrl: URL;
  wasmUrl: URL;
};
export declare const BROWSER_PACKAGE_VERSION: string;
export declare const CONTRACT_VERSIONS: Readonly<AdocWeaveContracts>;
export declare const CONFORMANCE_CONTRACT_VERSION: number;
export declare const CORE_API_VERSION: number;
export declare const CORE_PROFILE_VERSION: number;
export declare const HTML_CONTRACT_VERSION: number;
export declare const PROJECTION_CONTRACT_VERSION: number;
export declare const WASM_API_VERSION: number;

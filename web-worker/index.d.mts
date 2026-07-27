import type {
  AdocWeaveWasmResponse,
  Diagnostic,
  UpdateRequest,
} from "./protocol.generated.d.mts";

export type * from "./protocol.generated.d.mts";

export interface AdocWeaveResult {
  html: string;
  diagnostics: Diagnostic[];
  renderDiagnostics: Diagnostic[];
  sourceVersion: number;
  generation: number;
  packageVersion: string;
  result: AdocWeaveWasmResponse;
}

export interface AdocWeaveError {
  code: string;
  message: string;
  sourceVersion: number | null;
  generation: number;
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
export declare const PACKAGE_VERSION: string;

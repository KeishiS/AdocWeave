import type {
  AdocWeaveWasmResponse,
  AdocWeaveError,
  UpdateRequest,
} from "./protocol.generated.d.mts";

export type * from "./protocol.generated.d.mts";

export type AdocWeaveResult =
  Omit<AdocWeaveWasmResponse, "version"> & { sourceVersion: number };

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

export type AdocWeaveClientLifecycleErrorCode =
  | "cancelled"
  | "disposed"
  | "invalid-worker-response"
  | "superseded"
  | "unsupported-package-version"
  | "unsupported-worker-protocol"
  | "worker-failed";

export declare class AdocWeaveClientError<Code extends string = string> extends Error {
  constructor(error: {
    code: Code;
    message: string;
    sourceVersion: number | null;
    generation: number;
  });
  readonly code: Code;
  readonly sourceVersion: number | null;
  readonly generation: number;
}

export declare function isAdocWeaveClientLifecycleError(
  error: unknown,
): error is AdocWeaveClientError<AdocWeaveClientLifecycleErrorCode>;

export declare class AdocWeaveClient {
  constructor(options: AdocWeaveClientOptions);
  readonly ready: Promise<void>;
  analyze(request: UpdateRequest): Promise<AdocWeaveResult>;
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
export declare function analyzeOnce(
  clientOptions: AdocWeaveClientOptions,
  request: UpdateRequest,
): Promise<AdocWeaveResult>;
export declare const BROWSER_PACKAGE_VERSION: string;
export declare const PACKAGE_VERSION: string;

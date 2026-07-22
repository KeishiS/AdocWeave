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
  result: AdocWeaveWasmResponse;
}

export interface TextRange {
  start: number;
  end: number;
}

export interface SourceBlockProjection {
  sourceRange: TextRange;
  contentRange: TextRange;
  languageRange: TextRange | null;
  language: string | null;
  source: string;
}

export interface FormulaProjection {
  kind: "inline" | "block";
  language: "latex" | "typst";
  sourceRange: TextRange;
  contentRange: TextRange;
  source: string;
}

export interface DocumentProjection {
  contractVersion: number;
  sourceId: string | null;
  sourceBlocks: SourceBlockProjection[];
  formulas: FormulaProjection[];
  referenceEdges: unknown[];
  externalLinks: unknown[];
  searchableText: unknown;
  structure: unknown;
  catalogs: unknown;
  targets: unknown[];
  title: unknown;
}

export interface AdocWeaveWasmResponse {
  apiVersion: number;
  version: number;
  generation: number;
  conformanceContractVersion: number;
  parse: {
    profileVersion: number;
    blockCount: number;
    nodeCount: number;
    referenceCount: number;
  };
  syntax: string;
  ast: string;
  html: string;
  diagnostics: unknown[];
  renderDiagnostics: unknown[];
  symbols: unknown[];
  projection: DocumentProjection;
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
  renderInputs?: RenderInputs;
  options?: AdocWeaveOptions;
}

export interface RenderInputs {
  references?: ResolvedReference[];
  resources?: ResolvedResource[];
}

export interface ResolvedReference {
  sourceStart: number;
  sourceEnd: number;
  outcome:
    | { status: "resolved"; href: string; displayText?: string; notices?: ("fallback")[] }
    | {
        status: "failed";
        kind:
          | "missing-target"
          | "missing-anchor"
          | "ambiguous-target"
          | "outside-root"
          | "resolver-failure";
        message: string;
      };
}

export interface ResolvedResource {
  sourceStart: number;
  sourceEnd: number;
  outcome:
    | {
        status: "resolved";
        href: string;
        mediaType: string | null;
        byteLength: number | null;
      }
    | {
        status: "failed";
        kind:
          | "missing"
          | "outside-root"
          | "scheme-denied"
          | "permission-denied"
          | "resolver-failure";
        message: string;
      };
}

export interface AdocWeaveOptions {
  syntaxMode?: "permissive" | "strict";
  protectedAttributes?: Record<string, string>;
  urlPolicy?: {
    allowedSchemes?: string[];
    allowRelative?: boolean;
    allowResolvedRelative?: boolean;
    allowResolvedRootRelative?: boolean;
    allowDataUris?: boolean;
  };
  externalLinks?: {
    openInNewContext?: boolean;
    noreferrer?: boolean;
  };
  sourceLanguages?: {
    allowed?: string[] | null;
    unknown?: "preserve-sanitized" | "omit-class" | "diagnostic";
  };
  mathLanguages?: ("latex" | "typst")[];
  unresolvedReferences?: "target" | "label-only" | "hidden";
  resources?: {
    images?: boolean;
    media?: boolean;
  };
  limits?: Record<string, number>;
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

import type { ProductSet } from "./protocol.generated.d.mts";

export type { ProductSet } from "./protocol.generated.d.mts";

export interface AdocWeaveResult {
  html: string;
  diagnostics: unknown[];
  renderDiagnostics: unknown[];
  sourceVersion: number;
  generation: number;
  packageVersion: string;
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
  packageVersion: string;
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

export interface DocumentAttributeOccurrence {
  range: TextRange;
  nameRange: TextRange;
  valueRange: TextRange;
  name: string;
  rawValue: string;
  operation: "set" | "unset";
}

export interface ResourceQuery {
  purpose: "image" | "icon" | "audio" | "video" | "video-poster";
  form: "inline" | "block";
  ownerRange: TextRange;
  range: TextRange;
  targetRange: TextRange;
  target: string;
}

export interface AdocWeaveWasmResponse {
  packageVersion: string;
  version: number;
  generation: number;
  products: Required<ProductSet>;
  parse: {
    packageVersion: string;
    blockCount: number;
    nodeCount: number;
    referenceCount: number;
  };
  syntax: string;
  ast: string;
  html: string;
  attributeOccurrences: DocumentAttributeOccurrence[];
  resourceQueries: ResourceQuery[];
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
  products?: ProductSet;
  renderInputs?: RenderInputs;
  analysisOptions?: AnalysisOptions;
  renderPolicy?: RenderPolicy;
  outputLimits?: OutputLimits;
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
      };
}

export interface ResolvedResource {
  sourceStart: number;
  sourceEnd: number;
  outcome:
    | {
        status: "resolved";
        href: string;
        mediaType: string;
        byteLength: number | null;
      }
    | {
        status: "failed";
        kind:
          | "missing"
          | "outside-root"
          | "scheme-denied"
          | "permission-denied"
          | "media-type-unavailable"
          | "resolver-failure";
      };
}

export interface AnalysisOptions {
  syntax?: {
    syntaxMode?: "permissive" | "strict";
    limits?: AnalysisLimits;
  };
  diagnostics?: {
    protectedAttributes?: Record<string, string>;
    authoredUrls?: {
      allowedSchemes?: string[];
      allowRelative?: boolean;
    };
    maxDiagnostics?: number;
    rules?: Record<
      string,
      {
        enabled?: boolean;
        severity?: "error" | "warning" | "information" | "hint";
      }
    >;
  };
}

export interface AnalysisLimits {
  maxInputBytes?: number;
  maxLineBytes?: number;
  maxListDepth?: number;
  maxListContinuations?: number;
  maxBlockDepth?: number;
  maxInlineDepth?: number;
  maxFormulaBytes?: number;
  maxTableBytes?: number;
  maxTableCells?: number;
  maxTableColumns?: number;
  maxTableDepth?: number;
  maxCatalogEntries?: number;
  maxCatalogBytes?: number;
  maxBlocks?: number;
  maxNodes?: number;
  maxReferences?: number;
  maxAttributes?: number;
  maxAttributeExpansionDepth?: number;
  maxAttributeExpansionBytes?: number;
}

export interface RenderPolicy {
  activeUrls?: {
    allowedSchemes?: string[];
    allowAuthoredRelative?: boolean;
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
  documentMode?: "fragment" | "complete";
  stylesheets?: (
    | { kind: "inline"; css: string }
    | { kind: "external"; url: string }
  )[];
}

export interface OutputLimits {
  maxOutputBytes?: number;
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

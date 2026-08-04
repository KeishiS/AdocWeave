export interface ProcessorOptions {
  extensions?: readonly string[];
}

export interface TxtPosition {
  line: number;
  column: number;
}

export interface TxtLocation {
  start: TxtPosition;
  end: TxtPosition;
}

export interface TxtNode {
  type: string;
  raw: string;
  range: readonly [number, number];
  loc: TxtLocation;
  children?: readonly TxtNode[];
  value?: string;
  depth?: number;
  url?: string;
  ordered?: boolean;
  lang?: string | null;
}

export interface TxtDocumentNode extends TxtNode {
  type: "Document";
  children: readonly TxtNode[];
}

export interface TextlintMessage {
  readonly [key: string]: unknown;
}

export interface ProcessorFunctions {
  preProcess(source: string, filePath?: string): TxtDocumentNode;
  postProcess(
    messages: readonly TextlintMessage[],
    filePath?: string
  ): { messages: TextlintMessage[]; filePath: string };
}

export declare class Processor {
  constructor(options?: ProcessorOptions);
  availableExtensions(): string[];
  processor(extension: string): ProcessorFunctions;
}

declare const plugin: { Processor: typeof Processor };
export default plugin;

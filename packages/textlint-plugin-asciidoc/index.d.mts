import type {
  TextlintMessage,
  TextlintPluginOptions,
  TextlintPluginPostProcessResult,
  TextlintPluginPreProcessResult,
  TextlintPluginProcessor
} from "@textlint/types";

export type ProcessorOptions = TextlintPluginOptions & {
  extensions?: readonly string[];
};

export declare class Processor implements TextlintPluginProcessor {
  constructor(options?: ProcessorOptions);
  availableExtensions(): string[];
  processor(extension: string): {
    preProcess(
      source: string,
      filePath?: string
    ): TextlintPluginPreProcessResult | Promise<TextlintPluginPreProcessResult>;
    postProcess(
      messages: TextlintMessage[],
      filePath?: string
    ): TextlintPluginPostProcessResult | Promise<TextlintPluginPostProcessResult>;
  };
}

declare const plugin: { Processor: typeof Processor };
export default plugin;

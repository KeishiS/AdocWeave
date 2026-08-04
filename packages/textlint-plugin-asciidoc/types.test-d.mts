import type {
  TextlintPluginCreator,
  TextlintPluginProcessor,
  TextlintPluginProcessorConstructor
} from "@textlint/types";

import plugin, { Processor } from "./index.mjs";

const constructorContract: TextlintPluginProcessorConstructor = Processor;
const pluginContract: TextlintPluginCreator = plugin;
const processorContract: TextlintPluginProcessor = new Processor({
  extensions: [".adoc"]
});

void constructorContract;
void pluginContract;
void processorContract;

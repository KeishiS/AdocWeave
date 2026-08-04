import { materializeTxtAST } from "./adapter.mjs";
import { parseText as parseWithBundledWasm } from "./bridge.mjs";

const builtInExtensions = [".adoc", ".asciidoc", ".asc"];
const extensionPattern = /^\.[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$/;

function configuredExtensions(options) {
  if (options === undefined) return [];
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Processorのoptionはobjectで指定してください。");
  }
  const extensions = options.extensions ?? [];
  if (!Array.isArray(extensions)) {
    throw new TypeError("extensionsは拡張子の配列で指定してください。");
  }
  return extensions.map((extension) => {
    if (typeof extension !== "string" || !extensionPattern.test(extension)) {
      throw new TypeError(`拡張子の形式が不正です: ${String(extension)}`);
    }
    return extension.toLowerCase();
  });
}

function removeFix(message) {
  const { fix: _fix, ...diagnostic } = message;
  return diagnostic;
}

export function createProcessorClass(parseText) {
  if (typeof parseText !== "function") {
    throw new TypeError("parseTextは関数で指定してください。");
  }

  return class Processor {
    #extensions;

    constructor(options = {}) {
      if (arguments.length > 1) {
        throw new TypeError("Processorのconstructorはoptionsだけを受け取ります。");
      }
      this.#extensions = Object.freeze([
        ...new Set([...builtInExtensions, ...configuredExtensions(options)])
      ]);
    }

    availableExtensions() {
      return [...this.#extensions];
    }

    processor(extension) {
      const normalized = typeof extension === "string" ? extension.toLowerCase() : extension;
      if (!this.#extensions.includes(normalized)) {
        throw new Error(`未対応の拡張子です: ${String(extension)}`);
      }
      return {
        preProcess(source, filePath) {
          return materializeTxtAST(source, parseText(source, filePath));
        },
        postProcess(messages, filePath) {
          return {
            messages: messages.map(removeFix),
            filePath: filePath ?? "<text>"
          };
        }
      };
    }
  };
}

export const Processor = createProcessorClass(parseWithBundledWasm);

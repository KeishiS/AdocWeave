import { toTxtAST } from "./adapter.mjs";
import { projectText as projectWithBundledWasm } from "./bridge.mjs";

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

export class Processor {
  #extensions;
  #projectText;

  constructor(options = {}, internals = {}) {
    this.#extensions = Object.freeze([
      ...new Set([...builtInExtensions, ...configuredExtensions(options)])
    ]);
    this.#projectText = internals.projectText ?? projectWithBundledWasm;
  }

  availableExtensions() {
    return [...this.#extensions];
  }

  processor(extension) {
    const normalized = typeof extension === "string" ? extension.toLowerCase() : extension;
    if (!this.#extensions.includes(normalized)) {
      throw new Error(`未対応の拡張子です: ${String(extension)}`);
    }
    const projectText = this.#projectText;
    return {
      preProcess(source, filePath) {
        if (typeof source !== "string") {
          throw new TypeError("AsciiDocの入力は文字列で指定してください。");
        }
        return toTxtAST(source, projectText(source, filePath));
      },
      postProcess(messages, filePath) {
        return {
          messages: messages.map(removeFix),
          filePath: filePath ?? "<text>"
        };
      }
    };
  }
}

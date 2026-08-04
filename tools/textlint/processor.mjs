import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { toTxtAST } from "./adapter.mjs";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const releaseManifest = JSON.parse(
  readFileSync(new URL("../../release-manifest.json", import.meta.url), "utf8")
);
const require = createRequire(import.meta.url);

let bridge;
function loadBridge() {
  bridge ??= require(`${repositoryRoot}target/adocweave-textlint-wasm-node/adocweave_wasm.js`);
  return bridge;
}

export class Processor {
  availableExtensions() {
    return [".adoc", ".asciidoc", ".asc"];
  }

  processor(extension) {
    if (!this.availableExtensions().includes(extension)) {
      throw new Error(`未対応の拡張子です: ${extension}`);
    }
    return {
      preProcess(source, filePath) {
        const projection = loadBridge().projectText({
          packageVersion: releaseManifest.packageVersion,
          sourceId: filePath ?? null,
          source
        });
        return toTxtAST(source, projection);
      },
      postProcess(messages, filePath) {
        return { messages, filePath: filePath ?? "<text>" };
      }
    };
  }
}

export default { Processor };

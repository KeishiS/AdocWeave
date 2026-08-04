import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { createParseText } from "../../packages/textlint-plugin-asciidoc/bridge.mjs";
import { createProcessorClass } from "../../packages/textlint-plugin-asciidoc/processor.mjs";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const releaseManifest = JSON.parse(
  readFileSync(new URL("../../release-manifest.json", import.meta.url), "utf8")
);
const require = createRequire(import.meta.url);

let bridge;
function loadBridge() {
  bridge ??= require(
    `${repositoryRoot}target/adocweave-textlint-wasm-node/adocweave_textlint_wasm.js`
  );
  return bridge;
}

const parseText = createParseText({
  bridgeLoader: loadBridge,
  componentVersion: releaseManifest.packageVersion
});

// リポジトリ内の検査でも、配布するProcessorとTxtAST adapterをそのまま使います。
// WebAssemblyだけはpackage作成前の専用build成果物へ接続します。
export const Processor = createProcessorClass(parseText);

export default { Processor };

import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { Processor as PublicProcessor } from "../../packages/textlint-plugin-asciidoc/index.mjs";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const releaseManifest = JSON.parse(
  readFileSync(new URL("../../release-manifest.json", import.meta.url), "utf8")
);
const require = createRequire(import.meta.url);

let bridge;
function loadBridge() {
  bridge ??= require(
    `${repositoryRoot}target/adocweave-textlint-wasm-node/adocweave_textlint_wasm.js`,
  );
  return bridge;
}

function projectText(source, filePath) {
  return loadBridge().projectText({
    packageVersion: releaseManifest.packageVersion,
    sourceId: filePath ?? null,
    source
  });
}

export class Processor extends PublicProcessor {
  constructor(options = {}) {
    // リポジトリ内の検査でも、配布するProcessorとTxtAST adapterをそのまま使います。
    // WebAssemblyだけはpackage作成前の専用build成果物へ接続します。
    super(options, { projectText });
  }
}

export default { Processor };

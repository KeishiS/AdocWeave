import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const require = createRequire(import.meta.url);

test("文章校正用exportをBrowser成果物から分離する", () => {
  const browser = readFileSync(
    `${repositoryRoot}target/adocweave-wasm-dev/adocweave_wasm.js`,
    "utf8"
  );
  const textlint = readFileSync(
    `${repositoryRoot}target/adocweave-textlint-wasm-node/adocweave_textlint_wasm.js`,
    "utf8"
  );
  assert.doesNotMatch(browser, /projectText/);
  assert.match(textlint, /projectText/);
  assert.deepEqual(
    Object.keys(
      require(
        `${repositoryRoot}target/adocweave-textlint-wasm-node/adocweave_textlint_wasm.js`
      )
    ),
    ["projectText"]
  );
});

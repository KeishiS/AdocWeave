import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const require = createRequire(import.meta.url);
const packageVersion = JSON.parse(
  readFileSync(`${repositoryRoot}release-manifest.json`, "utf8")
).packageVersion;

function errorPayload(operation) {
  try {
    operation();
  } catch (error) {
    return JSON.parse(typeof error === "string" ? error : error.message);
  }
  assert.fail("WebAssembly呼出しが成功しました");
}

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

test("実WebAssembly境界がversionとrequest上限をcode付きで拒否する", () => {
  const { projectText } = require(
    `${repositoryRoot}target/adocweave-textlint-wasm-node/adocweave_textlint_wasm.js`
  );
  assert.equal(
    errorPayload(() => projectText({
      packageVersion: "0.0.0",
      source: "",
      sourceId: null
    })).code,
    "unsupported-api-version"
  );
  assert.equal(
    errorPayload(() => projectText({
      packageVersion,
      source: "x".repeat(10 * 1024 * 1024 + 1),
      sourceId: null
    })).code,
    "input-too-large"
  );
  assert.equal(
    errorPayload(() => projectText({
      packageVersion,
      source: "",
      sourceId: "x".repeat(4 * 1024 + 1)
    })).code,
    "invalid-request"
  );
});

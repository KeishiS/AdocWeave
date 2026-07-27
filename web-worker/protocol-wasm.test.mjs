import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const require = createRequire(import.meta.url);
const wasm = require(resolve(root, "target/adocweave-wasm-node/adocweave_wasm.js"));
const corpus = JSON.parse(
  readFileSync(resolve(root, "fixtures/protocol/request-corpus.json"), "utf8"),
);
const release = JSON.parse(readFileSync(resolve(root, "release-manifest.json"), "utf8"));

function currentRequest() {
  return {
    ...structuredClone(corpus.defaultRequest),
    packageVersion: release.packageVersion,
  };
}

function currentPreprocessRequest() {
  return {
    ...structuredClone(corpus.preprocessRequest),
    packageVersion: release.packageVersion,
  };
}

function wasmError(operation) {
  try {
    operation();
  } catch (error) {
    return JSON.parse(String(error));
  }
  assert.fail("WASM request unexpectedly succeeded");
}

test("generated wasm-bindgen accepts the current default requests", () => {
  const response = wasm.process(currentRequest());
  assert.equal(response.packageVersion, release.packageVersion);
  assert.equal(response.version, 1);
  assert.equal(response.generation, 1);

  const preprocessed = wasm.preprocess(currentPreprocessRequest());
  assert.equal(preprocessed.packageVersion, release.packageVersion);
  assert.equal(preprocessed.source, "included\n");
});

test("generated wasm-bindgen rejects unknown and missing fields", () => {
  for (const { path, value } of corpus.unknownFieldCases) {
    const request = currentRequest();
    setPointer(request, path, value);
    assert.equal(wasmError(() => wasm.process(request)).code, "invalid-request", path);
  }

  for (const field of ["packageVersion", "version", "generation", "source"]) {
    const request = currentRequest();
    delete request[field];
    assert.equal(wasmError(() => wasm.process(request)).code, "invalid-request", field);
  }

  const preprocess = currentPreprocessRequest();
  preprocess.options.unexpected = true;
  assert.equal(wasmError(() => wasm.preprocess(preprocess)).code, "invalid-request");
});

test("generated wasm-bindgen rejects old package versions", () => {
  const request = currentRequest();
  request.packageVersion = corpus.oldVersion;
  assert.equal(
    wasmError(() => wasm.process(request)).code,
    "unsupported-api-version",
  );

  const preprocess = currentPreprocessRequest();
  preprocess.packageVersion = corpus.oldVersion;
  assert.equal(
    wasmError(() => wasm.preprocess(preprocess)).code,
    "unsupported-api-version",
  );
});

function setPointer(rootValue, pointer, value) {
  const segments = pointer.split("/").slice(1).map((segment) =>
    segment.replaceAll("~1", "/").replaceAll("~0", "~")
  );
  const key = segments.pop();
  let target = rootValue;
  for (const segment of segments) {
    target[segment] ??= {};
    target = target[segment];
  }
  target[key] = structuredClone(value);
}

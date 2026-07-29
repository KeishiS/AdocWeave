import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { generateRustPreprocessInputs } from "./protocol-rust-codegen.mjs";

const schema = JSON.parse(
  await readFile(new URL("../protocol/public-api.json", import.meta.url), "utf8"),
);

test("preprocess Rust inputs are generated without a core dependency", () => {
  const generated = generateRustPreprocessInputs(schema);

  for (const name of [
    "WasmAnalysisPreprocessInput",
    "WasmPreprocessOptions",
    "WasmPreprocessRequest",
    "WasmResource",
    "WasmSafeMode",
  ]) {
    assert.match(generated, new RegExp(`\\b${name}\\b`));
  }
  assert.doesNotMatch(generated, /adocweave::/);
  assert.match(generated, /enable_includes: true/);
  assert.match(generated, /max_include_depth: 16/);
});

test("schema field and default changes deterministically change generated Rust", () => {
  const changed = structuredClone(schema);
  changed.preprocessDefinitions.PreprocessOptions.fields.push({
    json: "probeLimit",
    type: "u32",
    default: 7,
  });

  const generated = generateRustPreprocessInputs(changed);
  assert.match(generated, /pub probe_limit: u32/);
  assert.match(generated, /probe_limit: 7/);
});

test("unsupported shapes and unreachable declared inputs fail closed", () => {
  const unsupported = structuredClone(schema);
  unsupported.preprocessDefinitions.PreprocessOptions.fields[0].type = "number";
  assert.throws(
    () => generateRustPreprocessInputs(unsupported),
    /unsupported preprocess Rust field type number/,
  );

  const unreachable = structuredClone(schema);
  for (const contract of [
    unreachable.preprocessRequest,
    unreachable.definitions.AnalysisPreprocessInput,
  ]) {
    contract.fields.find(({ json }) => json === "resources").type =
      "Record<string, string>";
  }
  assert.throws(
    () => generateRustPreprocessInputs(unreachable),
    /exactly match reachable inputs/,
  );
});

test("set collection metadata is explicit and validated", () => {
  const invalid = structuredClone(schema);
  invalid.preprocessDefinitions.PreprocessOptions.fields
    .find(({ json }) => json === "allowedSchemes").collection = "ordered-set";
  assert.throws(
    () => generateRustPreprocessInputs(invalid),
    /unsupported collection/,
  );
});

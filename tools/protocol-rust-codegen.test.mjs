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
  changed.preprocessRequest.fields.push({
    json: "probeLimit",
    type: "u32",
    default: 7,
  });

  const generated = generateRustPreprocessInputs(changed);
  assert.match(generated, /pub probe_limit: u32/);
  assert.match(
    generated,
    /fn default_wasm_preprocess_request_probe_limit\(\) -> u32 \{\s+7\s+\}/,
  );
  assert.match(
    generated,
    /#\[serde\(default = "default_wasm_preprocess_request_probe_limit"\)\]/,
  );
  assert.doesNotMatch(generated, /#\[serde\(default\)\]\s+pub probe_limit/);
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

test("mixed required and defaulted fields deserialize through schema default helpers", () => {
  const changed = structuredClone(schema);
  changed.preprocessRequest.fields.push({
    json: "probeLimit",
    type: "u32",
    default: 7,
  });

  const generated = generateRustPreprocessInputs(changed);
  const helper = generated.match(
    /fn default_wasm_preprocess_request_probe_limit\(\) -> u32 \{\s+(\d+)\s+\}/,
  );
  assert.equal(helper?.[1], "7");

  const attribute = generated.match(
    /#\[serde\(default = "([^"]+)"\)\]\s+pub probe_limit: u32/,
  );
  assert.equal(attribute?.[1], "default_wasm_preprocess_request_probe_limit");
});

test("Rust field identifiers reject keywords, invalid characters, and collisions", () => {
  for (const [json, message] of [
    ["type", /invalid Rust identifier type/],
    ["bad-name", /not a supported JSON field name/],
  ]) {
    const changed = structuredClone(schema);
    changed.preprocessRequest.fields.push({ json, type: "u32", default: 1 });
    assert.throws(() => generateRustPreprocessInputs(changed), message);
  }

  const collision = structuredClone(schema);
  collision.preprocessRequest.fields.push(
    { json: "probeUrl", type: "u32", default: 2 },
    { json: "probeUrl", type: "u32", default: 1 },
  );
  assert.throws(
    () => generateRustPreprocessInputs(collision),
    /fields collide as Rust identifier probe_url/,
  );
});

test("Rust enum variants reject invalid values and transformed collisions", () => {
  for (const value of ["bad_value", "self"]) {
    const changed = structuredClone(schema);
    changed.enums.SafeMode.push(value);
    assert.throws(
      () => generateRustPreprocessInputs(changed),
      value === "self"
        ? /invalid Rust identifier Self/
        : /unsupported Rust enum value/,
    );
  }

  const collision = structuredClone(schema);
  collision.enums.SafeMode.push("server-mode", "serverMode");
  assert.throws(
    () => generateRustPreprocessInputs(collision),
    /enum values collide as Rust identifier ServerMode/,
  );
});

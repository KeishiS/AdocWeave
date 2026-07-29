import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  generateRustPreprocessInputs,
  generateRustRenderInputs,
  generateRustRequestEnums,
  generateRustRequestWire,
  generateRustResponseTypes,
  generateRustSharedTypes,
} from "./protocol-rust-codegen.mjs";

const schema = JSON.parse(
  await readFile(new URL("../protocol/public-api.json", import.meta.url), "utf8"),
);

test("request wire Rust DTOs are generated from exact reachable ownership", () => {
  const generated = generateRustRequestWire(schema);

  for (const name of [
    "WasmRequest",
    "WasmAnalysisOptions",
    "WasmSyntaxOptions",
    "WasmDiagnosticProfile",
    "WasmRuleSettings",
    "WasmRenderPolicy",
    "WasmOutputLimits",
    "WasmStylesheet",
    "WasmExternalLinkPolicy",
    "WasmSourceLanguagePolicy",
    "WasmResourceCapabilities",
    "WasmLimits",
    "WasmAuthoredUrlPolicy",
    "WasmActiveUrlPolicy",
  ]) {
    assert.match(generated, new RegExp(`\\b${name}\\b`));
  }
  assert.doesNotMatch(generated, /adocweave::/);
  assert.match(generated, /max_output_bytes: 52428800/);
  assert.match(
    generated,
    /math_languages: vec!\[WasmMathLanguage::Latex, WasmMathLanguage::Typst\]/,
  );
  assert.match(
    generated,
    /allowed_schemes: vec!\["http"\.to_owned\(\), "https"\.to_owned\(\)\]/,
  );
});

test("request wire schema defaults deterministically change generated Rust", () => {
  const changed = structuredClone(schema);
  changed.settings.OutputLimits.fields[0].default = 1234;
  changed.definitions.AuthoredUrlPolicy.fields[0].default.push("gemini");

  const generated = generateRustRequestWire(changed);
  assert.match(generated, /max_output_bytes: 1234/);
  assert.match(
    generated,
    /allowed_schemes: vec!\["http"\.to_owned\(\), "https"\.to_owned\(\), "gemini"\.to_owned\(\)\]/,
  );
});

test("request wire generation fails closed on ownership and incomplete defaults", () => {
  const ownership = structuredClone(schema);
  ownership.settings.AnalysisOptions.fields.push({
    json: "newPolicy",
    type: "NewPolicy",
    default: {},
  });
  ownership.definitions.NewPolicy = {
    unknownFields: "reject",
    fields: [],
  };
  assert.throws(
    () => generateRustRequestWire(ownership),
    /unowned reachable request wire Rust type NewPolicy/,
  );

  const missingDefault = structuredClone(schema);
  delete missingDefault.settings.OutputLimits.fields[0].default;
  assert.throws(
    () => generateRustRequestWire(missingDefault),
    /must be required or defaulted/,
  );

  const mismatchedDefault = structuredClone(schema);
  mismatchedDefault.settings.OutputLimits.fields[0].default = "large";
  assert.throws(
    () => generateRustRequestWire(mismatchedDefault),
    /default does not match u32/,
  );
});

test("request wire generation rejects unknown-field drift, recursion, and collisions", () => {
  const unknownFields = structuredClone(schema);
  unknownFields.settings.OutputLimits.unknownFields = "allow";
  assert.throws(
    () => generateRustRequestWire(unknownFields),
    /must be a request wire object that rejects unknown fields/,
  );

  const recursive = structuredClone(schema);
  recursive.settings.AnalysisOptions.fields.push({
    json: "cycle",
    type: "AnalysisOptions",
    default: {},
  });
  assert.throws(
    () => generateRustRequestWire(recursive),
    /infinitely sized cycle: AnalysisOptions -> AnalysisOptions/,
  );

  const collision = structuredClone(schema);
  collision.settings.OutputLimits.fields.push({
    json: "maxOutputBytes",
    type: "u32",
    default: 1,
  });
  assert.throws(
    () => generateRustRequestWire(collision),
    /fields collide as Rust identifier max_output_bytes/,
  );

  const tagCollision = structuredClone(schema);
  tagCollision.taggedUnions.Stylesheet.variants.inline.push({
    json: "kind",
    type: "string",
    required: true,
  });
  assert.throws(
    () => generateRustRequestWire(tagCollision),
    /field collides with tag kind/,
  );
});

test("request wire generation is deterministic across schema insertion order", () => {
  const reordered = structuredClone(schema);
  reordered.settings = Object.fromEntries(Object.entries(reordered.settings).reverse());
  reordered.definitions = Object.fromEntries(
    Object.entries(reordered.definitions).reverse(),
  );
  reordered.taggedUnions.Stylesheet.variants = Object.fromEntries(
    Object.entries(reordered.taggedUnions.Stylesheet.variants).reverse(),
  );

  assert.equal(
    generateRustRequestWire(reordered),
    generateRustRequestWire(schema),
  );
});

test("render input Rust types are generated without a core dependency", () => {
  const generated = generateRustRenderInputs(schema);

  for (const name of [
    "WasmRenderInputs",
    "WasmResolvedReference",
    "WasmReferenceOutcome",
    "WasmReferenceNotice",
    "WasmReferenceFailureKind",
    "WasmResolvedResource",
    "WasmResourceOutcome",
    "WasmResourceFailureKind",
  ]) {
    assert.match(generated, new RegExp(`\\b${name}\\b`));
  }
  assert.doesNotMatch(generated, /adocweave::/);
  assert.match(
    generated,
    /pub\(crate\) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991/,
  );
  assert.match(
    generated,
    /deserialize_with = "deserialize_optional_safe_integer"/,
  );
});

test("render input schema changes deterministically change generated Rust", () => {
  const changed = structuredClone(schema);
  changed.definitions.RenderInputs.fields.push({
    json: "referenceGroups",
    type: "ResolvedReference[]",
    default: [],
  });

  const generated = generateRustRenderInputs(changed);
  assert.match(
    generated,
    /pub reference_groups: Vec<WasmResolvedReference>/,
  );
});

test("render input generation fails closed for unsupported and colliding shapes", () => {
  const unsupported = structuredClone(schema);
  unsupported.definitions.ResolvedResource.fields[0].type = "number";
  assert.throws(
    () => generateRustRenderInputs(unsupported),
    /unsupported reachable render input Rust type number/,
  );

  const unreachable = structuredClone(schema);
  unreachable.definitions.RenderInputs.fields.pop();
  assert.throws(
    () => generateRustRenderInputs(unreachable),
    /exactly match reachable inputs/,
  );

  const collision = structuredClone(schema);
  collision.taggedUnions.ResolvedReferenceOutcome.variants.resolved.push({
    json: "status",
    type: "string",
    required: true,
  });
  assert.throws(
    () => generateRustRenderInputs(collision),
    /field collides with tag status/,
  );
});

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

test("Rust enum variants reject non-kebab values, keywords, and duplicates", () => {
  for (const value of ["bad_value", "serverMode", "self"]) {
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
  collision.enums.SafeMode.push("server-mode", "server-mode");
  assert.throws(
    () => generateRustPreprocessInputs(collision),
    /enum values collide as Rust identifier ServerMode/,
  );
});

test("shared Rust enums are generated once with schema-derived defaults", () => {
  const generated = generateRustSharedTypes(schema);
  assert.match(generated, /pub enum WasmMathLanguage/);
  assert.match(generated, /pub enum WasmSeverity/);
  assert.match(generated, /#\[default\]\s+Warning/);

  const changed = structuredClone(schema);
  changed.enums.Severity.push("critical");
  assert.match(generateRustSharedTypes(changed), /Critical/);

  const conflicting = structuredClone(schema);
  conflicting.definitions.SeverityProbe = {
    fields: [{ json: "severity", type: "Severity", default: "error" }],
  };
  assert.throws(
    () => generateRustSharedTypes(conflicting),
    /Severity has conflicting shared Rust defaults/,
  );
});

test("request Rust enums are generated from the exact reachable ownership set", () => {
  const generated = generateRustRequestEnums(schema);

  for (const name of [
    "WasmDocumentMode",
    "WasmSyntaxMode",
    "WasmUnknownSourceLanguage",
    "WasmUnresolvedReferencePresentation",
  ]) {
    assert.match(generated, new RegExp(`pub enum ${name}\\b`));
  }
  for (const name of [
    "WasmMathLanguage",
    "WasmReferenceFailureKind",
    "WasmReferenceNotice",
    "WasmResourceFailureKind",
    "WasmSafeMode",
    "WasmSeverity",
  ]) {
    assert.doesNotMatch(generated, new RegExp(`pub enum ${name}\\b`));
  }
  assert.match(generated, /pub enum WasmSyntaxMode \{\s+#\[default\]\s+Permissive,/);
  assert.match(generated, /pub enum WasmDocumentMode \{\s+#\[default\]\s+Fragment,/);
  assert.match(
    generated,
    /pub enum WasmUnknownSourceLanguage \{\s+#\[default\]\s+PreserveSanitized,/,
  );
  assert.match(
    generated,
    /pub enum WasmUnresolvedReferencePresentation \{\s+#\[default\]\s+Target,/,
  );

  const unreachable = structuredClone(schema);
  unreachable.settings.SyntaxOptions.fields
    .find(({ json }) => json === "syntaxMode").type = "string";
  assert.throws(
    () => generateRustRequestEnums(unreachable),
    /ownership must exactly match reachable enums/,
  );
});

test("request Rust enum schema mutations fail closed", () => {
  const missingDefault = structuredClone(schema);
  delete missingDefault.settings.SyntaxOptions.fields
    .find(({ json }) => json === "syntaxMode").default;
  assert.throws(
    () => generateRustRequestEnums(missingDefault),
    /SyntaxMode must have one unambiguous request Rust default/,
  );

  const invalidDefault = structuredClone(schema);
  invalidDefault.settings.SyntaxOptions.fields
    .find(({ json }) => json === "syntaxMode").default = "unknown";
  assert.throws(
    () => generateRustRequestEnums(invalidDefault),
    /SyntaxMode has an invalid request Rust default/,
  );

  const conflictingDefault = structuredClone(schema);
  conflictingDefault.settings.AnalysisOptions.fields.push({
    json: "fallbackSyntaxMode",
    type: "SyntaxMode",
    default: "strict",
  });
  assert.throws(
    () => generateRustRequestEnums(conflictingDefault),
    /SyntaxMode must have one unambiguous request Rust default/,
  );

  const collidingVariant = structuredClone(schema);
  collidingVariant.enums.DocumentMode.push("fragment");
  assert.throws(
    () => generateRustRequestEnums(collidingVariant),
    /DocumentMode enum values collide as Rust identifier Fragment/,
  );
});

test("request Rust enum generation is deterministic across object insertion order", () => {
  const reordered = structuredClone(schema);
  for (const namespace of ["enums", "settings", "definitions", "preprocessDefinitions"]) {
    reordered[namespace] = Object.fromEntries(
      Object.entries(reordered[namespace]).reverse(),
    );
  }
  reordered.request.fields.reverse();

  assert.equal(
    generateRustRequestEnums(reordered),
    generateRustRequestEnums(schema),
  );
});

test("response Rust types are generated from the complete reachable schema", () => {
  const generated = generateRustResponseTypes(schema);

  for (const name of [
    "WasmResponse",
    "ParseSummary",
    "WasmDiagnostic",
    "WasmDocumentProjection",
    "WasmReferenceKey",
    "WasmProjectedResolutionOutcome",
    "WasmTextRange",
  ]) {
    assert.match(generated, new RegExp(`\\b${name}\\b`));
  }
  assert.match(generated, /pub products: WasmProductSet/);
  assert.match(generated, /pub projection: Option<WasmDocumentProjection>/);
  assert.match(generated, /pub children: Vec<WasmDocumentSymbol>/);
  assert.match(generated, /tag = "status"/);
  assert.match(generated, /display_text: Option<String>/);
  assert.match(generated, /\/\/\/ A half-open UTF-8 byte range/);
  assert.match(generated, /pub severity: WasmSeverity/);
  assert.match(generated, /pub language: WasmMathLanguage/);
  assert.match(
    generated,
    /derive\(Clone, Debug, Default, serde::Deserialize, serde::Serialize, Eq, PartialEq\)\]\n#\[serde\(rename_all = "camelCase", deny_unknown_fields\)\]\npub struct WasmAttributeQueryProduct/,
  );
  assert.doesNotMatch(generated, /pub enum WasmSeverity/);
  assert.doesNotMatch(generated, /pub enum WasmMathLanguage/);
  assert.doesNotMatch(generated, /WasmAnalysisOptions/);
  assert.doesNotMatch(generated, /adocweave::/);

  const actual = [...generated.matchAll(/pub (?:struct|enum) ([A-Z][A-Za-z0-9]*)/g)]
    .map((match) => match[1])
    .sort();
  const expected = [...expectedResponseTypes(schema)]
    .filter((name) => !["MathLanguage", "ProductSet", "Severity"].includes(name))
    .map((name) => ({
      AdocWeaveWasmResponse: "WasmResponse",
      ParseSummary: "ParseSummary",
    })[name] ?? `Wasm${name}`)
    .sort();
  assert.deepEqual(actual, expected);
});

function expectedResponseTypes(value) {
  const contracts = {
    ...value.definitions,
    ...value.dtos,
    ...value.enums,
    ...value.taggedUnions,
    AdocWeaveWasmResponse: value.response,
    ProductSet: value.productSet,
  };
  const reached = new Set();
  const pending = ["AdocWeaveWasmResponse"];
  while (pending.length > 0) {
    const name = pending.pop();
    if (reached.has(name)) continue;
    reached.add(name);
    const contract = contracts[name];
    const fields = contract.variants
      ? Object.values(contract.variants).flat()
      : Array.isArray(contract)
        ? []
        : contract.fields;
    for (const field of fields) {
      for (const reference of field.type.match(/[A-Z][A-Za-z0-9]*/g) ?? []) {
        if (reference !== "Required" && contracts[reference] && !reached.has(reference)) {
          pending.push(reference);
        }
      }
    }
  }
  return reached;
}

test("response generation is deterministic and follows schema field changes", () => {
  const first = generateRustResponseTypes(schema);
  const second = generateRustResponseTypes(structuredClone(schema));
  assert.equal(first, second);

  const changed = structuredClone(schema);
  changed.response.fields.push({ json: "traceId", type: "string | null" });
  assert.match(
    generateRustResponseTypes(changed),
    /pub trace_id: Option<String>/,
  );

  const reordered = structuredClone(schema);
  const variants = reordered.taggedUnions.ReferenceKey.variants;
  reordered.taggedUnions.ReferenceKey.variants = Object.fromEntries(
    Object.entries(variants).reverse(),
  );
  assert.equal(generateRustResponseTypes(reordered), first);
});

test("response generation fails closed for unsupported reachable types", () => {
  const unsupported = structuredClone(schema);
  unsupported.response.fields.push({ json: "clock", type: "safeInteger" });
  assert.throws(
    () => generateRustResponseTypes(unsupported),
    /unsupported response Rust field type safeInteger/,
  );

  const missing = structuredClone(schema);
  missing.response.fields.push({ json: "missing", type: "MissingContract" });
  assert.throws(
    () => generateRustResponseTypes(missing),
    /unsupported reachable response Rust type MissingContract/,
  );

  for (const type of [
    "string | null | null",
    "Required<Required<ProductSet>>",
    "Required<Severity>",
    "TextRange[][]",
    " TextRange",
  ]) {
    const malformed = structuredClone(schema);
    malformed.response.fields.push({ json: "malformed", type });
    assert.throws(
      () => generateRustResponseTypes(malformed),
      /unsupported response Rust field type/,
      type,
    );
  }

  const invalidDescription = structuredClone(schema);
  invalidDescription.dtos.TextRange.description = "first\n/// injected";
  assert.throws(
    () => generateRustResponseTypes(invalidDescription),
    /TextRange has an invalid Rust documentation description/,
  );
});

test("response unions reject tag collisions and Rust identifier collisions", () => {
  const tagCollision = structuredClone(schema);
  tagCollision.taggedUnions.ReferenceKey.variants.local.push({
    json: "kind",
    type: "string",
  });
  assert.throws(
    () => generateRustResponseTypes(tagCollision),
    /field collides with tag kind/,
  );

  const fieldCollision = structuredClone(schema);
  fieldCollision.response.fields.push(
    { json: "traceId", type: "string" },
    { json: "traceId", type: "string" },
  );
  assert.throws(
    () => generateRustResponseTypes(fieldCollision),
    /fields collide as Rust identifier trace_id/,
  );

  const contractCollision = structuredClone(schema);
  contractCollision.dtos.ParseSummary = { fields: [] };
  assert.throws(
    () => generateRustResponseTypes(contractCollision),
    /duplicate response Rust contract ParseSummary/,
  );
});

test("response generation rejects infinitely sized types but permits Vec recursion", () => {
  const direct = structuredClone(schema);
  direct.definitions.DirectCycle = {
    fields: [{ json: "next", type: "DirectCycle | null" }],
  };
  direct.response.fields.push({ json: "cycle", type: "DirectCycle" });
  assert.throws(
    () => generateRustResponseTypes(direct),
    /infinitely sized cycle: DirectCycle -> DirectCycle/,
  );

  const indirect = structuredClone(schema);
  indirect.definitions.FirstCycle = {
    fields: [{ json: "next", type: "SecondCycle" }],
  };
  indirect.definitions.SecondCycle = {
    fields: [{ json: "next", type: "FirstCycle | null" }],
  };
  indirect.response.fields.push({ json: "cycle", type: "FirstCycle" });
  assert.throws(
    () => generateRustResponseTypes(indirect),
    /infinitely sized cycle:/,
  );

  const vector = structuredClone(schema);
  vector.definitions.TreeNode = {
    fields: [{ json: "children", type: "TreeNode[]" }],
  };
  vector.response.fields.push({ json: "tree", type: "TreeNode" });
  assert.match(
    generateRustResponseTypes(vector),
    /pub children: Vec<WasmTreeNode>/,
  );
});

test("response Default derives require an explicit complete schema default", () => {
  const generated = generateRustResponseTypes(schema);
  assert.match(
    generated,
    /derive\(Clone, Debug, Default, serde::Deserialize, serde::Serialize, Eq, PartialEq\)\]\n#\[serde\(rename_all = "camelCase", deny_unknown_fields\)\]\npub struct WasmAttributeQueryProduct/,
  );
  assert.doesNotMatch(
    generated,
    /derive\([^)]*Default[^)]*\)\]\n#\[serde\(rename_all = "camelCase", deny_unknown_fields\)\]\npub struct (?:WasmResponse|ParseSummary)/,
  );

  const incomplete = structuredClone(schema);
  incomplete.dtos.AttributeQueryProduct.fields.push({
    json: "language",
    type: "MathLanguage",
  });
  assert.throws(
    () => generateRustResponseTypes(incomplete),
    /AttributeQueryProduct outputDefault must cover every field exactly once/,
  );

  const mismatched = structuredClone(schema);
  mismatched.dtos.AttributeQueryProduct.outputDefault.bindings = ["not-empty"];
  assert.throws(
    () => generateRustResponseTypes(mismatched),
    /AttributeQueryProduct.bindings outputDefault does not match Rust Default/,
  );
});

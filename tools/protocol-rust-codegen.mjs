const RUST_NAMES = {
  AnalysisPreprocessInput: "WasmAnalysisPreprocessInput",
  PreprocessOptions: "WasmPreprocessOptions",
  PreprocessRequest: "WasmPreprocessRequest",
  PreprocessResource: "WasmResource",
  SafeMode: "WasmSafeMode",
};

const PREPROCESS_OUTPUT_RUST_TYPES = new Set([
  "PreprocessResponse",
  "SourceMapSegment",
  "SourceMapping",
  "WasmError",
]);

const RESPONSE_RUST_NAME_OVERRIDES = {
  AdocWeaveWasmResponse: "WasmResponse",
  ParseSummary: "ParseSummary",
  PreprocessResponse: "WasmPreprocessResponse",
  ProductSet: "WasmProductSet",
  SourceMapSegment: "WasmSourceMapSegment",
  WasmError: "WasmError",
};

const RESPONSE_EXTERNAL_TYPES = new Set([
  "MathLanguage",
  "ProductSet",
  "Severity",
]);

const SHARED_RUST_ENUMS = [
  "MathLanguage",
  "Severity",
];

const REQUEST_RUST_ENUMS = [
  "DocumentMode",
  "SyntaxMode",
  "UnknownSourceLanguage",
  "UnresolvedReferencePresentation",
];

const EXTERNAL_REQUEST_RUST_ENUMS = [
  "MathLanguage",
  "ReferenceFailureKind",
  "ReferenceNotice",
  "ResourceFailureKind",
  "SafeMode",
  "Severity",
];

const REQUEST_WIRE_OWNED_TYPES = new Set([
  "ActiveUrlPolicy",
  "AnalysisLimits",
  "AnalysisOptions",
  "AuthoredUrlPolicy",
  "DiagnosticProfile",
  "ExternalLinkPolicy",
  "OutputLimits",
  "RenderPolicy",
  "ResourceCapabilities",
  "RuleSettings",
  "SourceLanguagePolicy",
  "Stylesheet",
  "SyntaxOptions",
  "WasmRequest",
]);

const REQUEST_WIRE_EXTERNAL_TYPES = {
  AnalysisPreprocessInput: "WasmAnalysisPreprocessInput",
  DocumentMode: "WasmDocumentMode",
  MathLanguage: "WasmMathLanguage",
  ProductSet: "WasmProductSet",
  RenderInputs: "WasmRenderInputs",
  Severity: "WasmSeverity",
  SyntaxMode: "WasmSyntaxMode",
  UnknownSourceLanguage: "WasmUnknownSourceLanguage",
  UnresolvedReferencePresentation: "WasmUnresolvedReferencePresentation",
};

const REQUEST_WIRE_RUST_NAME_OVERRIDES = {
  AnalysisLimits: "WasmLimits",
  WasmRequest: "WasmRequest",
};

const RENDER_INPUT_RUST_NAMES = {
  CitationSegment: "WasmCitationSegment",
  GeneratedBibliography: "WasmGeneratedBibliography",
  GeneratedBibliographyEntry: "WasmGeneratedBibliographyEntry",
  ReferenceFailureKind: "WasmReferenceFailureKind",
  ReferenceNotice: "WasmReferenceNotice",
  RenderInputs: "WasmRenderInputs",
  ResolvedCitation: "WasmResolvedCitation",
  ResolvedCitationOutcome: "WasmCitationOutcome",
  ResolvedReference: "WasmResolvedReference",
  ResolvedReferenceOutcome: "WasmReferenceOutcome",
  ResolvedResource: "WasmResolvedResource",
  ResolvedResourceOutcome: "WasmResourceOutcome",
  ResourceFailureKind: "WasmResourceFailureKind",
};

const RUST_KEYWORDS = new Set([
  "Self", "abstract", "as", "async", "await", "become", "box", "break", "const",
  "continue", "crate", "do", "dyn", "else", "enum", "extern", "false", "final",
  "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match", "mod",
  "move", "mut", "override", "priv", "pub", "ref", "return", "self", "static",
  "struct", "super", "trait", "true", "try", "type", "typeof", "union", "unsafe",
  "unsized", "use", "virtual", "where", "while", "yield",
]);

export function generateRustPreprocessInputs(schema) {
  const contracts = {
    ...schema.preprocessDefinitions,
    AnalysisPreprocessInput: schema.definitions?.AnalysisPreprocessInput,
    PreprocessRequest: schema.preprocessRequest,
    SafeMode: schema.enums?.SafeMode,
  };
  for (const name of Object.keys(RUST_NAMES)) {
    if (!contracts[name]) throw new Error(`missing preprocess Rust contract ${name}`);
  }
  const reached = reachableTypes(
    ["PreprocessRequest", "AnalysisPreprocessInput"],
    contracts,
  );
  const expected = Object.keys(RUST_NAMES).sort();
  if (JSON.stringify([...reached].sort()) !== JSON.stringify(expected)) {
    throw new Error(
      `generated preprocess Rust types must exactly match reachable inputs: ${[...reached].sort().join(", ")}`,
    );
  }

  const safeModeDefault = contracts.PreprocessOptions.fields
    .find(({ type }) => type === "SafeMode")?.default;
  if (typeof safeModeDefault !== "string") {
    throw new Error("PreprocessOptions must declare the SafeMode default");
  }
  return [
    "use std::collections::{BTreeMap, BTreeSet};",
    rustEnum("SafeMode", schema.enums.SafeMode, safeModeDefault),
    rustObject("PreprocessResource", contracts.PreprocessResource),
    rustObject("PreprocessOptions", contracts.PreprocessOptions),
    rustObject("AnalysisPreprocessInput", contracts.AnalysisPreprocessInput),
    rustObject("PreprocessRequest", contracts.PreprocessRequest),
  ].join("\n\n");
}

export function generateRustPreprocessOutputs(schema) {
  const contracts = collectPreprocessOutputContracts(schema);
  const reached = reachableResponseTypes(
    ["PreprocessResponse", "WasmError"],
    contracts,
  );
  const expected = [...PREPROCESS_OUTPUT_RUST_TYPES].sort();
  if (JSON.stringify([...reached].sort()) !== JSON.stringify(expected)) {
    throw new Error(
      `generated preprocess Rust output types must exactly match reachable outputs: ${[...reached].sort().join(", ")}`,
    );
  }
  validateResponseRustNames(reached);
  validateSizedResponseTypes(reached, contracts);
  return [...reached]
    .sort()
    .map((name) => {
      const contract = contracts[name];
      if (Array.isArray(contract)) return rustResponseEnum(name, contract);
      return rustResponseObject(name, contract, reached, contracts);
    })
    .join("\n\n");
}

function collectPreprocessOutputContracts(schema) {
  const contracts = {};
  for (const [namespace, entries] of [
    ["preprocessDefinitions", schema.preprocessDefinitions],
    ["enums", schema.enums],
  ]) {
    if (!entries || typeof entries !== "object") {
      throw new Error(`missing preprocess Rust output contract namespace ${namespace}`);
    }
    for (const [name, contract] of Object.entries(entries)) {
      if (Object.hasOwn(contracts, name)) {
        throw new Error(`duplicate preprocess Rust output contract ${name}`);
      }
      contracts[name] = contract;
    }
  }
  return contracts;
}

export function generateRustSharedTypes(schema) {
  return SHARED_RUST_ENUMS
    .map((name) => {
      const values = schema.enums?.[name];
      const defaultValue = sharedEnumDefault(schema, name);
      return rustSharedEnum(name, values, defaultValue);
    })
    .join("\n\n");
}

export function generateRustRequestEnums(schema) {
  const contracts = collectRequestContracts(schema);
  const reached = reachableRequestTypes(["WasmRequest"], contracts);
  const reachedEnums = [...reached]
    .filter((name) => Array.isArray(contracts[name]))
    .sort();
  const expectedEnums = [...REQUEST_RUST_ENUMS, ...EXTERNAL_REQUEST_RUST_ENUMS].sort();
  if (JSON.stringify(reachedEnums) !== JSON.stringify(expectedEnums)) {
    throw new Error(
      `request Rust enum ownership must exactly match reachable enums: ${reachedEnums.join(", ")}`,
    );
  }

  validateRequestRustEnumNames(reachedEnums);
  return [...REQUEST_RUST_ENUMS]
    .sort()
    .map((name) => {
      const values = contracts[name];
      const defaultValue = requestEnumDefault(name, reached, contracts);
      return rustRequestEnum(name, values, defaultValue);
    })
    .join("\n\n");
}

export function generateRustRequestWire(schema) {
  const contracts = collectRequestContracts(schema);
  const reached = reachableOwnedRequestWireTypes(
    ["WasmRequest"],
    contracts,
  );
  const owned = [...reached]
    .filter((name) => REQUEST_WIRE_OWNED_TYPES.has(name))
    .sort();
  const external = [...reached]
    .filter((name) => Object.hasOwn(REQUEST_WIRE_EXTERNAL_TYPES, name))
    .sort();
  const expectedOwned = [...REQUEST_WIRE_OWNED_TYPES].sort();
  const expectedExternal = Object.keys(REQUEST_WIRE_EXTERNAL_TYPES).sort();
  if (JSON.stringify(owned) !== JSON.stringify(expectedOwned)) {
    throw new Error(
      `request wire Rust ownership must exactly match reachable generated types: ${owned.join(", ")}`,
    );
  }
  if (JSON.stringify(external) !== JSON.stringify(expectedExternal)) {
    throw new Error(
      `request wire Rust ownership must exactly match reachable external types: ${external.join(", ")}`,
    );
  }
  validateRequestWireRustNames(reached);
  validateSizedRequestWireTypes(reached, contracts);

  const definitions = owned.map((name) => {
    const contract = contracts[name];
    if (contract.variants) {
      return rustRequestWireUnion(name, contract, reached, contracts);
    }
    return rustRequestWireObject(name, contract, reached, contracts);
  }).join("\n\n");
  const imports = external
    .map((name) => REQUEST_WIRE_EXTERNAL_TYPES[name])
    .sort();
  const importLines = [];
  for (let index = 0; index < imports.length; index += 4) {
    importLines.push(`    ${imports.slice(index, index + 4).join(", ")},`);
  }
  return `use std::collections::BTreeMap;

use crate::{
${importLines.join("\n")}
};

${definitions}`;
}

function reachableOwnedRequestWireTypes(roots, contracts) {
  const reached = new Set();
  const pending = [...roots];
  while (pending.length > 0) {
    const name = pending.pop();
    if (reached.has(name)) continue;
    const contract = contracts[name];
    if (!contract) {
      throw new Error(`unsupported reachable request wire Rust type ${name}`);
    }
    if (!REQUEST_WIRE_OWNED_TYPES.has(name)
        && !Object.hasOwn(REQUEST_WIRE_EXTERNAL_TYPES, name)) {
      throw new Error(`unowned reachable request wire Rust type ${name}`);
    }
    reached.add(name);
    if (Object.hasOwn(REQUEST_WIRE_EXTERNAL_TYPES, name)) continue;
    const fields = contract.variants
      ? Object.values(contract.variants).flat()
      : contract.fields;
    if (!Array.isArray(fields)) {
      throw new Error(`invalid request wire Rust contract ${name}`);
    }
    for (const field of fields) {
      validateRequestWireField(field, name);
      for (const reference of requestWireTypeReferences(field.type)) {
        if (!contracts[reference]) {
          throw new Error(`unsupported reachable request wire Rust type ${reference}`);
        }
        if (!reached.has(reference)) pending.push(reference);
      }
    }
  }
  return reached;
}

function parseRequestWireType(type) {
  if (typeof type !== "string" || type !== type.trim()) {
    throw new Error(`unsupported request wire Rust field type ${String(type)}`);
  }
  const nullable = type.match(/^(.+) \| null$/);
  if (nullable) {
    return { kind: "nullable", inner: parseRequestWireType(nullable[1]) };
  }
  const array = type.match(/^(.+)\[\]$/);
  if (array) {
    return { kind: "array", inner: parseRequestWireType(array[1]) };
  }
  const record = type.match(/^Record<string, (.+)>$/);
  if (record) {
    return { kind: "record", value: parseRequestWireType(record[1]) };
  }
  if (["boolean", "string", "u32"].includes(type)) {
    return { kind: "primitive", name: type };
  }
  if (/^[A-Z][A-Za-z0-9]*$/.test(type)) {
    return { kind: "named", name: type };
  }
  throw new Error(`unsupported request wire Rust field type ${type}`);
}

function requestWireTypeReferences(type) {
  const references = [];
  const visit = (parsed) => {
    if (parsed.kind === "named") references.push(parsed.name);
    if (parsed.kind === "array" || parsed.kind === "nullable") visit(parsed.inner);
    if (parsed.kind === "record") visit(parsed.value);
  };
  visit(parseRequestWireType(type));
  return references;
}

function validateRequestWireRustNames(reached) {
  const names = new Map();
  for (const schemaName of reached) {
    const rustName = requestWireRustName(schemaName);
    validateRustIdentifier(rustName, `request wire type ${schemaName}`);
    const previous = names.get(rustName);
    if (previous) {
      throw new Error(
        `request wire types ${previous} and ${schemaName} collide as Rust identifier ${rustName}`,
      );
    }
    names.set(rustName, schemaName);
  }
}

function requestWireRustName(name) {
  return REQUEST_WIRE_EXTERNAL_TYPES[name]
    ?? REQUEST_WIRE_RUST_NAME_OVERRIDES[name]
    ?? `Wasm${name}`;
}

function validateSizedRequestWireTypes(reached, contracts) {
  const directEdges = new Map();
  for (const name of reached) {
    if (!REQUEST_WIRE_OWNED_TYPES.has(name)) continue;
    const contract = contracts[name];
    const fields = contract.variants
      ? Object.values(contract.variants).flat()
      : contract.fields;
    directEdges.set(
      name,
      fields
        .map(({ type }) => directRequestWireReference(parseRequestWireType(type)))
        .filter((reference) =>
          reference
          && REQUEST_WIRE_OWNED_TYPES.has(reference)
          && !Array.isArray(contracts[reference])
        ),
    );
  }
  const visiting = new Set();
  const visited = new Set();
  const path = [];
  const visit = (name) => {
    if (visiting.has(name)) {
      const start = path.indexOf(name);
      throw new Error(
        `request wire Rust types have an infinitely sized cycle: ${[...path.slice(start), name].join(" -> ")}`,
      );
    }
    if (visited.has(name)) return;
    visiting.add(name);
    path.push(name);
    for (const next of directEdges.get(name) ?? []) visit(next);
    path.pop();
    visiting.delete(name);
    visited.add(name);
  };
  for (const name of directEdges.keys()) visit(name);
}

function directRequestWireReference(parsed) {
  if (parsed.kind === "named") return parsed.name;
  if (parsed.kind === "nullable") return directRequestWireReference(parsed.inner);
  return null;
}

function validateRequestWireField(field, owner) {
  validateField(field, owner);
  parseRequestWireType(field.type);
}

function rustRequestWireObject(name, contract, reached, contracts) {
  if (!contract || contract.unknownFields !== "reject" || !Array.isArray(contract.fields)) {
    throw new Error(`${name} must be a request wire object that rejects unknown fields`);
  }
  const allDefaulted = contract.fields.every((field) => Object.hasOwn(field, "default"));
  const defaultExpressions = allDefaulted
    ? contract.fields.map((field) =>
      rustRequestWireDefault(field.default, field.type, contracts)
    )
    : [];
  const deriveDefault = allDefaulted
    && defaultExpressions.every(requestWireExpressionUsesRustDefault);
  const rustFields = new Set();
  const helpers = [];
  const fields = contract.fields.map((field) => {
    validateRequestWireField(field, name);
    const identifier = rustField(field.json);
    validateRustIdentifier(identifier, `${name}.${field.json}`);
    if (rustFields.has(identifier)) {
      throw new Error(`${name} fields collide as Rust identifier ${identifier}`);
    }
    rustFields.add(identifier);
    let attribute = "";
    if (!allDefaulted && Object.hasOwn(field, "default")) {
      const helper = `default_${rustField(requestWireRustName(name)).replace(/^_/, "")}_${identifier}`;
      const type = rustRequestWireType(field.type, reached);
      const value = rustRequestWireDefault(field.default, field.type, contracts);
      helpers.push(`fn ${helper}() -> ${type} {
    ${value}
}`);
      attribute = `    #[serde(default = "${helper}")]\n`;
    }
    return `${attribute}    pub ${identifier}: ${rustRequestWireType(field.type, reached)},`;
  });
  const derives = [
    "Clone",
    ...(requestWireObjectIsCopy(name, contract, reached, contracts, new Set()) ? ["Copy"] : []),
    "Debug",
    ...(deriveDefault ? ["Default"] : []),
    "serde::Deserialize",
    "serde::Serialize",
    "Eq",
    "PartialEq",
  ];
  const serde = allDefaulted
    ? '#[serde(default, rename_all = "camelCase", deny_unknown_fields)]'
    : '#[serde(rename_all = "camelCase", deny_unknown_fields)]';
  let definition = `#[derive(${derives.join(", ")})]
${serde}
pub struct ${requestWireRustName(name)} {
${fields.join("\n")}
}`;
  if (allDefaulted && !deriveDefault) {
    const defaults = contract.fields.map(
      (field, index) => `            ${rustField(field.json)}: ${defaultExpressions[index]},`,
    );
    definition += `

impl Default for ${requestWireRustName(name)} {
    fn default() -> Self {
        Self {
${defaults.join("\n")}
        }
    }
}`;
  }
  return helpers.length === 0 ? definition : `${helpers.join("\n\n")}

${definition}`;
}

function requestWireExpressionUsesRustDefault(expression) {
  return expression === "None"
    || expression === "false"
    || expression === "0"
    || expression === "vec![]"
    || expression === "BTreeMap::new()"
    || expression === "Default::default()"
    || expression === '"".to_owned()';
}

function rustRequestWireUnion(name, contract, reached, contracts) {
  if (typeof contract.tag !== "string"
      || !/^[a-z][A-Za-z0-9]*$/.test(contract.tag)
      || contract.unknownFields !== "reject"
      || !contract.variants
      || Object.keys(contract.variants).length === 0) {
    throw new Error(`invalid request wire Rust tagged union ${name}`);
  }
  const variants = new Set();
  const helpers = [];
  const members = Object.entries(contract.variants)
    .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
    .map(([value, fields]) => {
    const variant = rustVariant(value);
    validateRustIdentifier(variant, `${name} union variant ${JSON.stringify(value)}`);
    if (variants.has(variant)) {
      throw new Error(`${name} union variants collide as Rust identifier ${variant}`);
    }
    variants.add(variant);
    const rustFields = new Set();
    const generated = fields.map((field) => {
      validateRequestWireField(field, `${name}.${value}`);
      if (field.json === contract.tag) {
        throw new Error(`${name}.${value} field collides with tag ${contract.tag}`);
      }
      const identifier = rustField(field.json);
      validateRustIdentifier(identifier, `${name}.${value}.${field.json}`);
      if (rustFields.has(identifier)) {
        throw new Error(`${name}.${value} fields collide as Rust identifier ${identifier}`);
      }
      rustFields.add(identifier);
      let attribute = "";
      if (Object.hasOwn(field, "default")) {
        const helper = `default_${rustField(requestWireRustName(name)).replace(/^_/, "")}_${rustField(variant)}_${identifier}`;
        const type = rustRequestWireType(field.type, reached);
        const defaultValue = rustRequestWireDefault(field.default, field.type, contracts);
        helpers.push(`fn ${helper}() -> ${type} {
    ${defaultValue}
}`);
        attribute = `        #[serde(default = "${helper}")]\n`;
      }
      return `${attribute}        ${identifier}: ${rustRequestWireType(field.type, reached)},`;
    });
    if (generated.length === 0) return `    ${variant},`;
    if (generated.length === 1 && !generated[0].includes("#[")) {
      return `    ${variant} { ${generated[0].trim().replace(/,$/, "")} },`;
    }
    return `    ${variant} {\n${generated.join("\n")}\n    },`;
    });
  const definition = `#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(
    tag = "${contract.tag}",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ${requestWireRustName(name)} {
${members.join("\n")}
}`;
  return helpers.length === 0 ? definition : `${helpers.join("\n\n")}

${definition}`;
}

function rustRequestWireType(type, reached) {
  return rustRequestWireTypeFromAst(parseRequestWireType(type), reached, type);
}

function rustRequestWireTypeFromAst(parsed, reached, source) {
  if (parsed.kind === "nullable") {
    return `Option<${rustRequestWireTypeFromAst(parsed.inner, reached, source)}>`;
  }
  if (parsed.kind === "array") {
    return `Vec<${rustRequestWireTypeFromAst(parsed.inner, reached, source)}>`;
  }
  if (parsed.kind === "record") {
    return `BTreeMap<String, ${rustRequestWireTypeFromAst(parsed.value, reached, source)}>`;
  }
  if (parsed.kind === "primitive") {
    return { boolean: "bool", string: "String", u32: "u32" }[parsed.name];
  }
  if (parsed.kind === "named" && reached.has(parsed.name)) {
    return requestWireRustName(parsed.name);
  }
  throw new Error(`unsupported request wire Rust field type ${source}`);
}

function rustRequestWireDefault(value, type, contracts) {
  const parsed = parseRequestWireType(type);
  validateRequestWireDefault(value, parsed, contracts, type, new Set());
  return rustRequestWireDefaultExpression(value, parsed, contracts);
}

function validateRequestWireDefault(value, parsed, contracts, source, visiting) {
  if (parsed.kind === "nullable") {
    if (value === null) return;
    return validateRequestWireDefault(value, parsed.inner, contracts, source, visiting);
  }
  if (parsed.kind === "array") {
    if (!Array.isArray(value)) {
      throw new Error(`request wire default does not match ${source}`);
    }
    for (const item of value) {
      validateRequestWireDefault(item, parsed.inner, contracts, source, visiting);
    }
    return;
  }
  if (parsed.kind === "record") {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new Error(`request wire default does not match ${source}`);
    }
    for (const item of Object.values(value)) {
      validateRequestWireDefault(item, parsed.value, contracts, source, visiting);
    }
    return;
  }
  if (parsed.kind === "primitive") {
    const valid = parsed.name === "boolean"
      ? typeof value === "boolean"
      : parsed.name === "string"
        ? typeof value === "string"
        : Number.isInteger(value) && value >= 0 && value <= 0xffff_ffff;
    if (!valid) throw new Error(`request wire default does not match ${source}`);
    return;
  }
  const contract = contracts[parsed.name];
  if (Array.isArray(contract)) {
    if (typeof value !== "string" || !contract.includes(value)) {
      throw new Error(`request wire default does not match ${source}`);
    }
    return;
  }
  if (parsed.name === "ProductSet" && value === "browser-default") return;
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`request wire default does not match ${source}`);
  }
  if (visiting.has(parsed.name)) {
    throw new Error(`recursive request wire default for ${parsed.name}`);
  }
  visiting.add(parsed.name);
  const fields = new Map(contract.fields.map((field) => [field.json, field]));
  for (const [fieldName, fieldValue] of Object.entries(value)) {
    const field = fields.get(fieldName);
    if (!field) throw new Error(`request wire default has unknown field ${parsed.name}.${fieldName}`);
    validateRequestWireDefault(
      fieldValue,
      parseRequestWireType(field.type),
      contracts,
      field.type,
      visiting,
    );
  }
  for (const field of contract.fields) {
    if (!Object.hasOwn(value, field.json)
        && field.required === true
        && !Object.hasOwn(field, "default")) {
      throw new Error(`request wire default omits required field ${parsed.name}.${field.json}`);
    }
  }
  visiting.delete(parsed.name);
}

function rustRequestWireDefaultExpression(value, parsed, contracts) {
  if (parsed.kind === "nullable") {
    return value === null
      ? "None"
      : `Some(${rustRequestWireDefaultExpression(value, parsed.inner, contracts)})`;
  }
  if (parsed.kind === "array") {
    const values = value.map((item) =>
      rustRequestWireDefaultExpression(item, parsed.inner, contracts)
    );
    return `vec![${values.join(", ")}]`;
  }
  if (parsed.kind === "record") {
    if (Object.keys(value).length !== 0) {
      throw new Error("non-empty request wire record defaults are unsupported");
    }
    return "BTreeMap::new()";
  }
  if (parsed.kind === "primitive") {
    if (parsed.name === "string") return `${JSON.stringify(value)}.to_owned()`;
    return String(value);
  }
  const contract = contracts[parsed.name];
  if (Array.isArray(contract)) {
    return `${requestWireRustName(parsed.name)}::${rustVariant(value)}`;
  }
  return "Default::default()";
}

function requestWireObjectIsCopy(name, contract, reached, contracts, visiting) {
  if (visiting.has(name)) return false;
  visiting.add(name);
  const copy = contract.fields.every((field) =>
    requestWireTypeIsCopy(parseRequestWireType(field.type), reached, contracts, visiting)
  );
  visiting.delete(name);
  return copy;
}

function requestWireTypeIsCopy(parsed, reached, contracts, visiting) {
  if (parsed.kind === "nullable") {
    return requestWireTypeIsCopy(parsed.inner, reached, contracts, visiting);
  }
  if (parsed.kind === "array" || parsed.kind === "record") return false;
  if (parsed.kind === "primitive") return parsed.name !== "string";
  if (!reached.has(parsed.name)) return false;
  if (Object.hasOwn(REQUEST_WIRE_EXTERNAL_TYPES, parsed.name)) {
    return [
      "DocumentMode",
      "MathLanguage",
      "Severity",
      "SyntaxMode",
      "UnknownSourceLanguage",
      "UnresolvedReferencePresentation",
    ].includes(parsed.name);
  }
  const contract = contracts[parsed.name];
  if (Array.isArray(contract)) return true;
  if (contract.variants) return false;
  return requestWireObjectIsCopy(parsed.name, contract, reached, contracts, visiting);
}

export function generateRustRenderInputs(schema) {
  const contracts = {
    CitationSegment: schema.definitions?.CitationSegment,
    GeneratedBibliography: schema.definitions?.GeneratedBibliography,
    GeneratedBibliographyEntry: schema.definitions?.GeneratedBibliographyEntry,
    ReferenceFailureKind: schema.enums?.ReferenceFailureKind,
    ReferenceNotice: schema.enums?.ReferenceNotice,
    RenderInputs: schema.definitions?.RenderInputs,
    ResolvedCitation: schema.definitions?.ResolvedCitation,
    ResolvedCitationOutcome: schema.taggedUnions?.ResolvedCitationOutcome,
    ResolvedReference: schema.definitions?.ResolvedReference,
    ResolvedReferenceOutcome: schema.taggedUnions?.ResolvedReferenceOutcome,
    ResolvedResource: schema.definitions?.ResolvedResource,
    ResolvedResourceOutcome: schema.taggedUnions?.ResolvedResourceOutcome,
    ResourceFailureKind: schema.enums?.ResourceFailureKind,
  };
  const reached = reachableRenderInputTypes(["RenderInputs"], contracts);
  const expected = Object.keys(RENDER_INPUT_RUST_NAMES).sort();
  if (JSON.stringify([...reached].sort()) !== JSON.stringify(expected)) {
    throw new Error(
      `generated render input Rust types must exactly match reachable inputs: ${[...reached].sort().join(", ")}`,
    );
  }
  validateRenderInputRustNames(expected);

  return [
    `pub(crate) const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn deserialize_optional_safe_integer<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<u64> as serde::Deserialize>::deserialize(deserializer)?;
    if value.is_some_and(|value| value > MAX_SAFE_INTEGER) {
        return Err(serde::de::Error::custom(
            "safe integer exceeds the JavaScript maximum",
        ));
    }
    Ok(value)
}`,
    renderInputEnum("ReferenceNotice", contracts.ReferenceNotice),
    renderInputEnum("ReferenceFailureKind", contracts.ReferenceFailureKind),
    renderInputEnum("ResourceFailureKind", contracts.ResourceFailureKind),
    renderInputUnion(
      "ResolvedReferenceOutcome",
      contracts.ResolvedReferenceOutcome,
      reached,
    ),
    renderInputUnion(
      "ResolvedResourceOutcome",
      contracts.ResolvedResourceOutcome,
      reached,
    ),
    renderInputUnion(
      "ResolvedCitationOutcome",
      contracts.ResolvedCitationOutcome,
      reached,
    ),
    renderInputObject("CitationSegment", contracts.CitationSegment, reached),
    renderInputObject("GeneratedBibliographyEntry", contracts.GeneratedBibliographyEntry, reached),
    renderInputObject("GeneratedBibliography", contracts.GeneratedBibliography, reached),
    renderInputObject("ResolvedCitation", contracts.ResolvedCitation, reached),
    renderInputObject("ResolvedReference", contracts.ResolvedReference, reached),
    renderInputObject("ResolvedResource", contracts.ResolvedResource, reached),
    renderInputObject("RenderInputs", contracts.RenderInputs, reached),
  ].join("\n\n");
}

function reachableRenderInputTypes(roots, contracts) {
  const reached = new Set();
  const pending = [...roots];
  while (pending.length > 0) {
    const name = pending.pop();
    if (reached.has(name)) continue;
    if (!contracts[name]) {
      throw new Error(`unsupported reachable render input Rust type ${name}`);
    }
    reached.add(name);
    const contract = contracts[name];
    const fields = contract.variants
      ? Object.values(contract.variants).flat()
      : Array.isArray(contract)
        ? []
        : contract.fields;
    if (!Array.isArray(fields)) {
      throw new Error(`invalid render input Rust contract ${name}`);
    }
    for (const field of fields) {
      if (!field || typeof field.type !== "string") {
        throw new Error(`invalid render input Rust field in ${name}`);
      }
      for (const reference of renderInputTypeReferences(field.type)) {
        if (!contracts[reference]) {
          throw new Error(`unsupported reachable render input Rust type ${reference}`);
        }
        if (!reached.has(reference)) pending.push(reference);
      }
    }
  }
  return reached;
}

function renderInputTypeReferences(type) {
  if (type !== type.trim()) {
    throw new Error(`unsupported render input Rust field type ${JSON.stringify(type)}`);
  }
  const references = type.match(/[A-Za-z][A-Za-z0-9]*/g) ?? [];
  const builtins = new Set(["null", "safeInteger", "string", "u32"]);
  return references.filter((reference) => !builtins.has(reference));
}

function validateRenderInputRustNames(names) {
  const rustNames = new Set();
  for (const name of names) {
    const rustName = RENDER_INPUT_RUST_NAMES[name];
    validateRustIdentifier(rustName, `render input type ${name}`);
    if (rustNames.has(rustName)) {
      throw new Error(`render input types collide as Rust identifier ${rustName}`);
    }
    rustNames.add(rustName);
  }
}

function renderInputEnum(name, values) {
  if (!Array.isArray(values) || values.length === 0) {
    throw new Error(`${name} must have at least one render input Rust enum value`);
  }
  const identifiers = new Set();
  const variants = values.map((value) => {
    const variant = rustVariant(value);
    validateRustIdentifier(variant, `${name} enum value ${JSON.stringify(value)}`);
    if (identifiers.has(variant)) {
      throw new Error(`${name} enum values collide as Rust identifier ${variant}`);
    }
    identifiers.add(variant);
    return `    ${variant},`;
  });
  return `#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ${RENDER_INPUT_RUST_NAMES[name]} {
${variants.join("\n")}
}`;
}

function renderInputObject(name, contract, reached) {
  if (!contract || contract.unknownFields !== "reject" || !Array.isArray(contract.fields)) {
    throw new Error(`${name} must be a render input object that rejects unknown fields`);
  }
  const allDefaulted = contract.fields.every((field) => Object.hasOwn(field, "default"));
  const derives = [
    "Clone",
    "Debug",
    ...(allDefaulted && contract.fields.every(fieldUsesRustDefault) ? ["Default"] : []),
    "serde::Deserialize",
    "serde::Serialize",
    "Eq",
    "PartialEq",
  ];
  const fields = renderInputFields(name, contract.fields, reached);
  const serde = allDefaulted
    ? '#[serde(default, rename_all = "camelCase", deny_unknown_fields)]'
    : '#[serde(rename_all = "camelCase", deny_unknown_fields)]';
  return `#[derive(${derives.join(", ")})]
${serde}
pub struct ${RENDER_INPUT_RUST_NAMES[name]} {
${fields.join("\n")}
}`;
}

function renderInputUnion(name, contract, reached) {
  if (!contract
      || typeof contract.tag !== "string"
      || !/^[a-z][A-Za-z0-9]*$/.test(contract.tag)
      || contract.unknownFields !== "reject"
      || !contract.variants
      || Object.keys(contract.variants).length === 0) {
    throw new Error(`invalid render input Rust tagged union ${name}`);
  }
  const variantNames = new Set();
  const variants = Object.entries(contract.variants).map(([value, fields]) => {
    const variant = rustVariant(value);
    validateRustIdentifier(variant, `${name} union variant ${JSON.stringify(value)}`);
    if (variantNames.has(variant)) {
      throw new Error(`${name} union variants collide as Rust identifier ${variant}`);
    }
    variantNames.add(variant);
    if (fields.some((field) => field.json === contract.tag)) {
      throw new Error(`${name}.${value} field collides with tag ${contract.tag}`);
    }
    const members = renderInputFields(`${name}.${value}`, fields, reached, 8, false);
    return members.length === 0
      ? `    ${variant},`
      : `    ${variant} {\n${members.join("\n")}\n    },`;
  });
  return `#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(
    tag = "${contract.tag}",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ${RENDER_INPUT_RUST_NAMES[name]} {
${variants.join("\n")}
}`;
}

function renderInputFields(owner, fields, reached, indentation = 4, isPublic = true) {
  const rustFields = new Set();
  return fields.map((field) => {
    validateField(field, owner);
    const identifier = rustField(field.json);
    validateRustIdentifier(identifier, `${owner}.${field.json}`);
    if (rustFields.has(identifier)) {
      throw new Error(`${owner} fields collide as Rust identifier ${identifier}`);
    }
    rustFields.add(identifier);
    const attributes = [];
    if (Object.hasOwn(field, "default")) attributes.push("default");
    if (field.type === "safeInteger | null") {
      attributes.push('deserialize_with = "deserialize_optional_safe_integer"');
    }
    const prefix = " ".repeat(indentation);
    const attribute = attributes.length > 0
      ? `${prefix}#[serde(${attributes.join(", ")})]\n`
      : "";
    const visibility = isPublic ? "pub " : "";
    return `${attribute}${prefix}${visibility}${identifier}: ${renderInputRustType(field.type, reached)},`;
  });
}

function renderInputRustType(type, reached) {
  if (type === "string") return "String";
  if (type === "string | null") return "Option<String>";
  if (type === "safeInteger | null") return "Option<u64>";
  if (type === "u32") return "u32";
  const nullable = type.match(/^([A-Za-z][A-Za-z0-9]*) \| null$/);
  if (nullable && reached.has(nullable[1])) {
    return `Option<${RENDER_INPUT_RUST_NAMES[nullable[1]]}>`;
  }
  const array = type.match(/^([A-Za-z][A-Za-z0-9]*)\[\]$/);
  if (array && reached.has(array[1])) {
    return `Vec<${RENDER_INPUT_RUST_NAMES[array[1]]}>`;
  }
  if (reached.has(type)) return RENDER_INPUT_RUST_NAMES[type];
  throw new Error(`unsupported render input Rust field type ${type}`);
}

function collectRequestContracts(schema) {
  const contracts = {
    WasmRequest: schema.request,
    ProductSet: schema.productSet,
  };
  for (const [namespace, entries] of [
    ["enums", schema.enums],
    ["settings", schema.settings],
    ["definitions", schema.definitions],
    ["preprocessDefinitions", schema.preprocessDefinitions],
    ["taggedUnions", schema.taggedUnions],
  ]) {
    if (!entries || typeof entries !== "object" || Array.isArray(entries)) {
      throw new Error(`missing request Rust contract namespace ${namespace}`);
    }
    for (const [name, contract] of Object.entries(entries)) {
      if (Object.hasOwn(contracts, name)) {
        throw new Error(`duplicate request Rust contract ${name}`);
      }
      contracts[name] = contract;
    }
  }
  if (!contracts.WasmRequest || !contracts.ProductSet) {
    throw new Error("missing request Rust root contract");
  }
  return contracts;
}

function reachableRequestTypes(roots, contracts) {
  const reached = new Set();
  const pending = [...roots];
  while (pending.length > 0) {
    const name = pending.pop();
    if (reached.has(name)) continue;
    const contract = contracts[name];
    if (!contract) {
      throw new Error(`unsupported reachable request Rust type ${name}`);
    }
    reached.add(name);
    const fields = contract.variants
      ? Object.values(contract.variants).flat()
      : Array.isArray(contract)
        ? []
        : contract.fields;
    if (!Array.isArray(fields)) {
      throw new Error(`invalid request Rust contract ${name}`);
    }
    for (const field of fields) {
      if (!field || typeof field.type !== "string") {
        throw new Error(`invalid request Rust field in ${name}`);
      }
      for (const reference of requestTypeReferences(field.type)) {
        if (!contracts[reference]) {
          throw new Error(`unsupported reachable request Rust type ${reference}`);
        }
        if (!reached.has(reference)) pending.push(reference);
      }
    }
  }
  return reached;
}

function requestTypeReferences(type) {
  if (type !== type.trim()) {
    throw new Error(`unsupported request Rust field type ${JSON.stringify(type)}`);
  }
  const references = type.match(/[A-Za-z][A-Za-z0-9]*/g) ?? [];
  const builtins = new Set([
    "Record",
    "Required",
    "SharedArrayBuffer",
    "boolean",
    "null",
    "number",
    "safeInteger",
    "string",
    "u32",
    "unknown",
  ]);
  return references.filter((reference) => !builtins.has(reference));
}

function requestEnumDefault(name, reached, contracts) {
  const defaults = [];
  for (const owner of [...reached].sort()) {
    const contract = contracts[owner];
    if (Array.isArray(contract)) continue;
    const fields = contract.variants
      ? Object.values(contract.variants).flat()
      : contract.fields;
    for (const field of fields) {
      if (field.type !== name || !Object.hasOwn(field, "default")) continue;
      defaults.push(field.default);
    }
  }
  if (defaults.length === 0
      || defaults.some((value) => typeof value !== "string")
      || new Set(defaults).size !== 1) {
    throw new Error(`${name} must have one unambiguous request Rust default`);
  }
  const [defaultValue] = defaults;
  if (!contracts[name].includes(defaultValue)) {
    throw new Error(`${name} has an invalid request Rust default`);
  }
  return defaultValue;
}

function validateRequestRustEnumNames(names) {
  const rustNames = new Set();
  for (const name of names) {
    const rustName = requestRustName(name);
    validateRustIdentifier(rustName, `request enum type ${name}`);
    if (rustNames.has(rustName)) {
      throw new Error(`request enum types collide as Rust identifier ${rustName}`);
    }
    rustNames.add(rustName);
  }
}

function requestRustName(name) {
  return `Wasm${name}`;
}

function rustRequestEnum(name, values, defaultValue) {
  if (!Array.isArray(values) || values.length === 0) {
    throw new Error(`${name} must have at least one request Rust enum value`);
  }
  const identifiers = new Set();
  const variants = values.map((value) => {
    const variant = rustVariant(value);
    validateRustIdentifier(variant, `${name} enum value ${JSON.stringify(value)}`);
    if (identifiers.has(variant)) {
      throw new Error(`${name} enum values collide as Rust identifier ${variant}`);
    }
    identifiers.add(variant);
    return value === defaultValue
      ? `    #[default]\n    ${variant},`
      : `    ${variant},`;
  });
  return `#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ${requestRustName(name)} {
${variants.join("\n")}
}`;
}

function sharedEnumDefault(schema, name) {
  const contracts = [
    ...Object.values(schema.settings ?? {}),
    ...Object.values(schema.definitions ?? {}),
    ...Object.values(schema.preprocessDefinitions ?? {}),
    schema.request,
    schema.preprocessRequest,
  ].filter(Boolean);
  const defaults = new Set(
    contracts
      .flatMap((contract) => contract.fields ?? [])
      .filter((field) => field.type === name && Object.hasOwn(field, "default"))
      .map((field) => field.default),
  );
  if ([...defaults].some((value) => typeof value !== "string") || defaults.size > 1) {
    throw new Error(`${name} has conflicting shared Rust defaults`);
  }
  return defaults.values().next().value;
}

function rustSharedEnum(name, values, defaultValue) {
  if (!Array.isArray(values) || values.length === 0) {
    throw new Error(`${name} must have at least one shared Rust enum value`);
  }
  if (defaultValue !== undefined && !values.includes(defaultValue)) {
    throw new Error(`${name} has an invalid shared Rust default`);
  }
  const identifiers = new Set();
  const variants = values.map((value) => {
    const variant = rustVariant(value);
    validateRustIdentifier(variant, `${name} enum value ${JSON.stringify(value)}`);
    if (identifiers.has(variant)) {
      throw new Error(`${name} enum values collide as Rust identifier ${variant}`);
    }
    identifiers.add(variant);
    return value === defaultValue
      ? `    #[default]\n    ${variant},`
      : `    ${variant},`;
  });
  const derives = [
    "Clone",
    "Copy",
    "Debug",
    ...(defaultValue === undefined ? [] : ["Default"]),
    "serde::Deserialize",
    "serde::Serialize",
    "Eq",
    "PartialEq",
  ];
  return `#[derive(${derives.join(", ")})]
#[serde(rename_all = "kebab-case")]
pub enum ${responseRustName(name)} {
${variants.join("\n")}
}`;
}

export function generateRustResponseTypes(schema) {
  const contracts = collectResponseContracts(schema);
  const reached = reachableResponseTypes(["AdocWeaveWasmResponse"], contracts);
  validateResponseRustNames(reached);
  validateSizedResponseTypes(reached, contracts);

  const definitions = [...reached]
    .filter((name) => !RESPONSE_EXTERNAL_TYPES.has(name))
    .sort()
    .map((name) => {
      const contract = contracts[name];
      if (Array.isArray(contract)) return rustResponseEnum(name, contract);
      if (contract.variants) return rustResponseUnion(name, contract, reached);
      return rustResponseObject(name, contract, reached, contracts);
    })
    .join("\n\n");
  const imports = [...reached]
    .filter((name) => RESPONSE_EXTERNAL_TYPES.has(name))
    .map(responseRustName)
    .sort();
  return imports.length === 0
    ? definitions
    : `use crate::{${imports.join(", ")}};\n\n${definitions}`;
}

function collectResponseContracts(schema) {
  const contracts = {};
  for (const [namespace, entries] of [
    ["definitions", schema.definitions],
    ["dtos", schema.dtos],
    ["enums", schema.enums],
    ["taggedUnions", schema.taggedUnions],
    ["roots", {
      AdocWeaveWasmResponse: schema.response,
      ProductSet: schema.productSet,
    }],
  ]) {
    if (!entries || typeof entries !== "object") {
      throw new Error(`missing response Rust contract namespace ${namespace}`);
    }
    for (const [name, contract] of Object.entries(entries)) {
      if (Object.hasOwn(contracts, name)) {
        throw new Error(`duplicate response Rust contract ${name}`);
      }
      contracts[name] = contract;
    }
  }
  return contracts;
}

function reachableResponseTypes(roots, contracts) {
  const reached = new Set();
  const pending = [...roots];
  while (pending.length > 0) {
    const name = pending.pop();
    if (reached.has(name)) continue;
    const contract = contracts[name];
    if (!contract) {
      throw new Error(`unsupported reachable response Rust type ${name}`);
    }
    reached.add(name);
    const fields = contract.variants
      ? Object.values(contract.variants).flat()
      : Array.isArray(contract)
        ? []
        : contract.fields;
    if (!Array.isArray(fields)) {
      throw new Error(`invalid response Rust contract ${name}`);
    }
    for (const field of fields) {
      validateResponseField(field, name);
      for (const reference of responseTypeReferences(field.type)) {
        if (!contracts[reference]) {
          throw new Error(`unsupported reachable response Rust type ${reference}`);
        }
        if (!reached.has(reference)) pending.push(reference);
      }
    }
  }
  return reached;
}

function validateSizedResponseTypes(reached, contracts) {
  const directEdges = new Map();
  for (const name of reached) {
    if (RESPONSE_EXTERNAL_TYPES.has(name)) continue;
    const contract = contracts[name];
    const fields = contract?.variants
      ? Object.values(contract.variants).flat()
      : Array.isArray(contract)
        ? []
        : contract?.fields ?? [];
    directEdges.set(
      name,
      fields
        .map(({ type }) => directResponseReference(parseResponseType(type)))
        .filter((reference) =>
          reference
          && reached.has(reference)
          && !RESPONSE_EXTERNAL_TYPES.has(reference)
          && !Array.isArray(contracts[reference])
        ),
    );
  }

  const visiting = new Set();
  const visited = new Set();
  const path = [];
  const visit = (name) => {
    if (visiting.has(name)) {
      const start = path.indexOf(name);
      throw new Error(
        `response Rust types have an infinitely sized cycle: ${[...path.slice(start), name].join(" -> ")}`,
      );
    }
    if (visited.has(name)) return;
    visiting.add(name);
    path.push(name);
    for (const next of directEdges.get(name) ?? []) visit(next);
    path.pop();
    visiting.delete(name);
    visited.add(name);
  };
  for (const name of directEdges.keys()) visit(name);
}

function directResponseReference(parsed) {
  if (parsed.kind === "named") return parsed.name;
  if (parsed.kind === "nullable" || parsed.kind === "required") {
    return directResponseReference(parsed.inner);
  }
  return null;
}

function responseTypeReferences(type) {
  const parsed = parseResponseType(type);
  if (parsed.kind === "named") return [parsed.name];
  if (parsed.kind === "array" || parsed.kind === "nullable" || parsed.kind === "required") {
    return responseTypeReferencesFromAst(parsed.inner);
  }
  return [];
}

function responseTypeReferencesFromAst(parsed) {
  if (parsed.kind === "named") return [parsed.name];
  if (parsed.kind === "array" || parsed.kind === "nullable" || parsed.kind === "required") {
    return responseTypeReferencesFromAst(parsed.inner);
  }
  return [];
}

function parseResponseType(type) {
  if (typeof type !== "string" || type !== type.trim()) {
    throw new Error(`unsupported response Rust field type ${String(type)}`);
  }
  if (type === "Required<ProductSet>") {
    return {
      kind: "required",
      inner: { kind: "named", name: "ProductSet" },
    };
  }
  const nullable = type.match(/^([A-Za-z][A-Za-z0-9]*) \| null$/);
  if (nullable) {
    return { kind: "nullable", inner: parseResponseAtom(nullable[1], type) };
  }
  const array = type.match(/^([A-Za-z][A-Za-z0-9]*)\[\]$/);
  if (array) {
    return { kind: "array", inner: parseResponseAtom(array[1], type) };
  }
  return parseResponseAtom(type, type);
}

function parseResponseAtom(value, source) {
  if (["string", "u32", "boolean"].includes(value)) {
    return { kind: "primitive", name: value };
  }
  if (/^[A-Z][A-Za-z0-9]*$/.test(value)) {
    return { kind: "named", name: value };
  }
  throw new Error(`unsupported response Rust field type ${source}`);
}

function validateResponseRustNames(reached) {
  const names = new Map();
  for (const schemaName of reached) {
    const rustName = responseRustName(schemaName);
    validateRustIdentifier(rustName, `response type ${schemaName}`);
    const previous = names.get(rustName);
    if (previous) {
      throw new Error(
        `response types ${previous} and ${schemaName} collide as Rust identifier ${rustName}`,
      );
    }
    names.set(rustName, schemaName);
  }
}

function responseRustName(name) {
  return RESPONSE_RUST_NAME_OVERRIDES[name] ?? `Wasm${name}`;
}

function rustResponseEnum(name, values) {
  if (!Array.isArray(values) || values.length === 0) {
    throw new Error(`${name} must have at least one response enum value`);
  }
  const identifiers = new Set();
  const variants = values.map((value) => {
    const variant = rustVariant(value);
    validateRustIdentifier(variant, `${name} enum value ${JSON.stringify(value)}`);
    if (identifiers.has(variant)) {
      throw new Error(`${name} enum values collide as Rust identifier ${variant}`);
    }
    identifiers.add(variant);
    return `    ${variant},`;
  });
  return `#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ${responseRustName(name)} {
${variants.join("\n")}
}`;
}

function rustResponseObject(name, contract, reached, contracts) {
  if (!contract || !Array.isArray(contract.fields)) {
    throw new Error(`invalid response Rust object ${name}`);
  }
  if (contract.description !== undefined
      && (typeof contract.description !== "string"
        || contract.description.length === 0
        || /[\r\n]/.test(contract.description))) {
    throw new Error(`${name} has an invalid Rust documentation description`);
  }
  const rustFields = new Set();
  const fields = contract.fields.map((field) => {
    validateResponseField(field, name);
    const identifier = rustField(field.json);
    validateRustIdentifier(identifier, `${name}.${field.json}`);
    if (rustFields.has(identifier)) {
      throw new Error(`${name} fields collide as Rust identifier ${identifier}`);
    }
    rustFields.add(identifier);
    return `    pub ${identifier}: ${rustResponseType(field.type, reached)},`;
  });
  const derives = [
    "Clone",
    ...(responseObjectIsCopy(name, contract, reached, contracts, new Set()) ? ["Copy"] : []),
    "Debug",
    ...(responseObjectHasExplicitDefault(name, contract) ? ["Default"] : []),
    "serde::Deserialize",
    "serde::Serialize",
    "Eq",
    "PartialEq",
  ];
  const documentation = contract.description
    ? `/// ${contract.description}\n`
    : "";
  return `${documentation}#[derive(${derives.join(", ")})]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ${responseRustName(name)} {
${fields.join("\n")}
}`;
}

function rustResponseUnion(name, contract, reached) {
  if (typeof contract.tag !== "string"
      || !/^[a-z][A-Za-z0-9]*$/.test(contract.tag)
      || !contract.variants
      || Object.keys(contract.variants).length === 0) {
    throw new Error(`invalid response Rust tagged union ${name}`);
  }
  const variants = Object.entries(contract.variants)
    .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
    .map(([value, fields]) => {
      const variant = rustVariant(value);
      validateRustIdentifier(variant, `${name} union variant ${JSON.stringify(value)}`);
      const rustFields = new Set();
      const members = fields.map((field) => {
        validateResponseField(field, `${name}.${value}`);
        if (field.json === contract.tag) {
          throw new Error(`${name}.${value} field collides with tag ${contract.tag}`);
        }
        const identifier = rustField(field.json);
        validateRustIdentifier(identifier, `${name}.${value}.${field.json}`);
        if (rustFields.has(identifier)) {
          throw new Error(
            `${name}.${value} fields collide as Rust identifier ${identifier}`,
          );
        }
        rustFields.add(identifier);
        return `        ${identifier}: ${rustResponseType(field.type, reached)},`;
      });
      return members.length === 0
        ? `    ${variant},`
        : `    ${variant} {\n${members.join("\n")}\n    },`;
    });
  return `#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(
    tag = "${contract.tag}",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ${responseRustName(name)} {
${variants.join("\n")}
}`;
}

function validateResponseField(field, owner) {
  if (!field?.json || !field?.type) throw new Error(`${owner} has an invalid field`);
  if (!/^[a-z][A-Za-z0-9]*$/.test(field.json)) {
    throw new Error(`${owner}.${field.json} is not a supported JSON field name`);
  }
}

function rustResponseType(type, reached) {
  return rustResponseTypeFromAst(parseResponseType(type), reached, type);
}

function rustResponseTypeFromAst(parsed, reached, source) {
  if (parsed.kind === "required") {
    return rustResponseTypeFromAst(parsed.inner, reached, source);
  }
  if (parsed.kind === "nullable") {
    return `Option<${rustResponseTypeFromAst(parsed.inner, reached, source)}>`;
  }
  if (parsed.kind === "array") {
    return `Vec<${rustResponseTypeFromAst(parsed.inner, reached, source)}>`;
  }
  if (parsed.kind === "primitive") {
    return {
      string: "String",
      u32: "u32",
      boolean: "bool",
    }[parsed.name];
  }
  if (parsed.kind === "named" && reached.has(parsed.name)) {
    return responseRustName(parsed.name);
  }
  throw new Error(`unsupported response Rust field type ${source}`);
}

function responseObjectIsCopy(name, contract, reached, contracts, visiting) {
  if (visiting.has(name)) return false;
  visiting.add(name);
  const copy = contract.fields.every((field) =>
    responseTypeIsCopy(field.type, reached, contracts, visiting)
  );
  visiting.delete(name);
  return copy;
}

function responseObjectHasExplicitDefault(name, contract) {
  if (contract.outputDefault === undefined) return false;
  const value = contract.outputDefault;
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${name} must declare an object outputDefault`);
  }
  const expectedFields = contract.fields.map(({ json }) => json).sort();
  const defaultFields = Object.keys(value).sort();
  if (JSON.stringify(defaultFields) !== JSON.stringify(expectedFields)) {
    throw new Error(`${name} outputDefault must cover every field exactly once`);
  }
  for (const field of contract.fields) {
    if (!responseFieldUsesRustDefault(field.type, value[field.json])) {
      throw new Error(
        `${name}.${field.json} outputDefault does not match Rust Default`,
      );
    }
  }
  return true;
}

function responseFieldUsesRustDefault(type, value) {
  const parsed = parseResponseType(type);
  if (parsed.kind === "array") return Array.isArray(value) && value.length === 0;
  if (parsed.kind === "nullable") return value === null;
  if (parsed.kind !== "primitive") return false;
  if (parsed.name === "string") return value === "";
  if (parsed.name === "u32") return value === 0;
  if (parsed.name === "boolean") return value === false;
  return false;
}

function responseTypeIsCopy(type, reached, contracts, visiting) {
  return responseTypeAstIsCopy(
    parseResponseType(type),
    reached,
    contracts,
    visiting,
  );
}

function responseTypeAstIsCopy(parsed, reached, contracts, visiting) {
  if (parsed.kind === "required" || parsed.kind === "nullable") {
    return responseTypeAstIsCopy(parsed.inner, reached, contracts, visiting);
  }
  if (parsed.kind === "array") return false;
  if (parsed.kind === "primitive") return parsed.name !== "string";
  const value = parsed.name;
  if (!reached.has(value)) return false;
  if (RESPONSE_EXTERNAL_TYPES.has(value)) return true;
  const contract = contracts[value];
  if (Array.isArray(contract)) return true;
  if (contract?.variants) return false;
  return responseObjectIsCopy(value, contract, reached, contracts, visiting);
}

function reachableTypes(roots, contracts) {
  const reached = new Set();
  const pending = [...roots];
  while (pending.length > 0) {
    const name = pending.pop();
    if (reached.has(name)) continue;
    if (!RUST_NAMES[name]) throw new Error(`unsupported reachable preprocess Rust type ${name}`);
    reached.add(name);
    const contract = contracts[name];
    if (Array.isArray(contract)) continue;
    if (!contract || !Array.isArray(contract.fields)) {
      throw new Error(`invalid preprocess Rust contract ${name}`);
    }
    for (const field of contract.fields) {
      for (const reference of field.type.match(/[A-Z][A-Za-z0-9]*/g) ?? []) {
        if (contracts[reference] && !reached.has(reference)) pending.push(reference);
      }
    }
  }
  return reached;
}

function rustEnum(name, values, defaultValue) {
  if (!Array.isArray(values) || values.length === 0 || !values.includes(defaultValue)) {
    throw new Error(`${name} must have a valid default`);
  }
  const identifiers = new Set();
  const variants = values.map((value) => {
    const variant = rustVariant(value);
    validateRustIdentifier(variant, `${name} enum value ${JSON.stringify(value)}`);
    if (identifiers.has(variant)) {
      throw new Error(`${name} enum values collide as Rust identifier ${variant}`);
    }
    identifiers.add(variant);
    return value === defaultValue ? `    #[default]\n    ${variant},` : `    ${variant},`;
  });
  return `#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ${RUST_NAMES[name]} {
${variants.join("\n")}
}`;
}

function rustObject(name, contract) {
  if (contract.unknownFields !== "reject") {
    throw new Error(`${name} must reject unknown fields`);
  }
  const allDefaulted = contract.fields.every((field) => Object.hasOwn(field, "default"));
  const deriveDefault = allDefaulted && contract.fields.every(fieldUsesRustDefault);
  const derives = [
    "Clone",
    "Debug",
    ...(deriveDefault ? ["Default"] : []),
    "serde::Deserialize",
    "serde::Serialize",
    "Eq",
    "PartialEq",
  ];
  const serde = allDefaulted
    ? '#[serde(default, rename_all = "camelCase", deny_unknown_fields)]'
    : '#[serde(rename_all = "camelCase", deny_unknown_fields)]';
  const rustFields = new Set();
  const helpers = [];
  const fields = contract.fields.map((field) => {
    validateField(field, name);
    const identifier = rustField(field.json);
    validateRustIdentifier(identifier, `${name}.${field.json}`);
    if (rustFields.has(identifier)) {
      throw new Error(`${name} fields collide as Rust identifier ${identifier}`);
    }
    rustFields.add(identifier);
    let defaultAttribute = "";
    if (!allDefaulted && Object.hasOwn(field, "default")) {
      const helper = rustDefaultHelper(name, identifier);
      defaultAttribute = `    #[serde(default = "${helper}")]\n`;
      helpers.push(`fn ${helper}() -> ${rustType(field)} {
    ${rustDefault(field)}
}`);
    }
    return `${defaultAttribute}    pub ${identifier}: ${rustType(field)},`;
  });
  const definition = `#[derive(${derives.join(", ")})]
${serde}
pub struct ${RUST_NAMES[name]} {
${fields.join("\n")}
}`;
  if (!allDefaulted) {
    return helpers.length === 0 ? definition : `${helpers.join("\n\n")}

${definition}`;
  }
  if (deriveDefault) return definition;
  const defaults = contract.fields.map(
    (field) => `            ${rustField(field.json)}: ${rustDefault(field)},`,
  );
  return `${definition}

impl Default for ${RUST_NAMES[name]} {
    fn default() -> Self {
        Self {
${defaults.join("\n")}
        }
    }
}`;
}

function fieldUsesRustDefault(field) {
  const value = field.default;
  return value === null
    || (Array.isArray(value) && value.length === 0)
    || (value && typeof value === "object" && Object.keys(value).length === 0)
    || value === false
    || value === 0;
}

function validateField(field, owner) {
  if (!field.json || !field.type) throw new Error(`${owner} has an invalid field`);
  if (!/^[a-z][A-Za-z0-9]*$/.test(field.json)) {
    throw new Error(`${owner}.${field.json} is not a supported JSON field name`);
  }
  if (field.required !== true && !Object.hasOwn(field, "default")) {
    throw new Error(`${owner}.${field.json} must be required or defaulted`);
  }
  if (field.collection !== undefined
      && (field.collection !== "set" || field.type !== "string[]")) {
    throw new Error(`${owner}.${field.json} has an unsupported collection`);
  }
}

function rustType(field) {
  if (field.collection === "set") return "BTreeSet<String>";
  const type = field.type;
  if (type === "string") return "String";
  if (type === "string | null") return "Option<String>";
  if (type === "u32") return "u32";
  if (type === "boolean") return "bool";
  if (type === "string[]") return "Vec<String>";
  const record = type.match(/^Record<string, (.+)>$/);
  if (record) return `BTreeMap<String, ${rustType({ type: record[1] })}>`;
  if (RUST_NAMES[type]) return RUST_NAMES[type];
  throw new Error(`unsupported preprocess Rust field type ${type}`);
}

function rustDefault(field) {
  const value = field.default;
  if (value === null) return "None";
  if (Array.isArray(value) && value.length === 0) return "Default::default()";
  if (value && typeof value === "object" && Object.keys(value).length === 0) {
    return "Default::default()";
  }
  if (typeof value === "boolean" || typeof value === "number") return String(value);
  if (typeof value === "string" && RUST_NAMES[field.type]) {
    return `${RUST_NAMES[field.type]}::${rustVariant(value)}`;
  }
  if (typeof value === "string" && field.type === "string") {
    return `${JSON.stringify(value)}.to_owned()`;
  }
  throw new Error(`unsupported preprocess Rust default for ${field.json}`);
}

function rustField(value) {
  return value.replace(/[A-Z]/g, (character) => `_${character.toLowerCase()}`);
}

function rustVariant(value) {
  if (typeof value !== "string" || !/^[a-z][a-z0-9]*(?:-[a-z][a-z0-9]*)*$/.test(value)) {
    throw new Error(`unsupported Rust enum value ${JSON.stringify(value)}`);
  }
  return value
    .split("-")
    .map((part) => `${part[0].toUpperCase()}${part.slice(1)}`)
    .join("");
}

function rustDefaultHelper(owner, field) {
  return `default_${rustField(RUST_NAMES[owner]).replace(/^_/, "")}_${field}`;
}

function validateRustIdentifier(identifier, source) {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(identifier) || RUST_KEYWORDS.has(identifier)) {
    throw new Error(`${source} produces invalid Rust identifier ${identifier}`);
  }
}

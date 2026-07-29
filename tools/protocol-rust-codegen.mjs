const RUST_NAMES = {
  AnalysisPreprocessInput: "WasmAnalysisPreprocessInput",
  PreprocessOptions: "WasmPreprocessOptions",
  PreprocessRequest: "WasmPreprocessRequest",
  PreprocessResource: "WasmResource",
  SafeMode: "WasmSafeMode",
};

const RESPONSE_RUST_NAME_OVERRIDES = {
  AdocWeaveWasmResponse: "WasmResponse",
  ParseSummary: "ParseSummary",
  ProductSet: "WasmProductSet",
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

export function generateRustSharedTypes(schema) {
  return SHARED_RUST_ENUMS
    .map((name) => {
      const values = schema.enums?.[name];
      const defaultValue = sharedEnumDefault(schema, name);
      return rustSharedEnum(name, values, defaultValue);
    })
    .join("\n\n");
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
    ...(responseObjectIsDefault(name, contract, reached, contracts, new Set())
      ? ["Default"]
      : []),
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

function responseObjectIsDefault(name, contract, reached, contracts, visiting) {
  if (visiting.has(name)) return false;
  visiting.add(name);
  const hasDefault = contract.fields.every((field) =>
    responseTypeIsDefault(field.type, reached, contracts, visiting)
  );
  visiting.delete(name);
  return hasDefault;
}

function responseTypeIsDefault(type, reached, contracts, visiting) {
  const parsed = parseResponseType(type);
  if (parsed.kind === "nullable" || parsed.kind === "array") return true;
  if (parsed.kind === "required") {
    return responseTypeAstIsDefault(parsed.inner, reached, contracts, visiting);
  }
  return responseTypeAstIsDefault(parsed, reached, contracts, visiting);
}

function responseTypeAstIsDefault(parsed, reached, contracts, visiting) {
  if (parsed.kind === "primitive") return true;
  const value = parsed.name;
  if (!reached.has(value)) return false;
  if (value === "ProductSet" || value === "Severity") return true;
  if (value === "MathLanguage") return false;
  const contract = contracts[value];
  if (Array.isArray(contract) || contract?.variants) return false;
  return responseObjectIsDefault(value, contract, reached, contracts, visiting);
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

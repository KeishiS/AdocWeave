const RUST_NAMES = {
  AnalysisPreprocessInput: "WasmAnalysisPreprocessInput",
  PreprocessOptions: "WasmPreprocessOptions",
  PreprocessRequest: "WasmPreprocessRequest",
  PreprocessResource: "WasmResource",
  SafeMode: "WasmSafeMode",
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

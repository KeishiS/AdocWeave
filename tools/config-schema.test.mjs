import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const schema = JSON.parse(readFileSync(new URL("../config/adocweave.schema.json", import.meta.url)));
const corpus = JSON.parse(
  readFileSync(new URL("../fixtures/config/schema-corpus.json", import.meta.url)),
);

/// Keywords this file knows how to evaluate.
///
/// The validator below is deliberately small: it exists to compare the schema
/// against the implementation over one corpus, not to be a JSON Schema
/// implementation. A keyword it cannot evaluate would be silently ignored and
/// the comparison would pass while saying nothing, so the schema is required to
/// stay inside this set.
const SUPPORTED = new Set([
  "$defs",
  "$id",
  "$ref",
  "$schema",
  "additionalProperties",
  "allOf",
  "const",
  "description",
  "enum",
  "if",
  "items",
  "maximum",
  "maxItems",
  "maxLength",
  "minLength",
  "minimum",
  "not",
  "oneOf",
  "pattern",
  "properties",
  "propertyNames",
  "required",
  "then",
  "title",
  "type",
  "uniqueItems",
]);

function keywords(node, found = new Set()) {
  if (Array.isArray(node)) {
    for (const item of node) keywords(item, found);
    return found;
  }
  if (node === null || typeof node !== "object") return found;
  for (const [key, value] of Object.entries(node)) {
    found.add(key);
    // Property names are data, not keywords.
    if (key === "properties" || key === "$defs") {
      for (const child of Object.values(value)) keywords(child, found);
    } else {
      keywords(value, found);
    }
  }
  return found;
}

function resolve(node) {
  if (node && typeof node === "object" && typeof node.$ref === "string") {
    const path = node.$ref.replace(/^#\//, "").split("/");
    return path.reduce((current, key) => current[key], schema);
  }
  return node;
}

function typeMatches(expected, value) {
  switch (expected) {
    case "object":
      return typeof value === "object" && value !== null && !Array.isArray(value);
    case "array":
      return Array.isArray(value);
    case "integer":
      return Number.isInteger(value);
    case "string":
      return typeof value === "string";
    case "boolean":
      return typeof value === "boolean";
    case "number":
      return typeof value === "number";
    default:
      throw new Error(`未知のtype: ${expected}`);
  }
}

/// Validates one value, returning the failures rather than throwing.
function validate(node, value, where = "") {
  const rules = resolve(node);
  const failures = [];
  if (rules.type !== undefined && !typeMatches(rules.type, value)) {
    return [`${where}: type ${rules.type} ではありません`];
  }
  if (rules.const !== undefined && value !== rules.const) {
    failures.push(`${where}: const ${rules.const} と一致しません`);
  }
  if (rules.enum !== undefined && !rules.enum.includes(value)) {
    failures.push(`${where}: enum に含まれません`);
  }
  if (rules.pattern !== undefined && !new RegExp(rules.pattern, "u").test(value)) {
    failures.push(`${where}: pattern に一致しません`);
  }
  const stringLength = typeof value === "string" ? [...value].length : undefined;
  if (rules.minLength !== undefined && stringLength < rules.minLength) {
    failures.push(`${where}: minLength を下回ります`);
  }
  if (rules.maxLength !== undefined && stringLength > rules.maxLength) {
    failures.push(`${where}: maxLength を超えます`);
  }
  if (rules.minimum !== undefined && value < rules.minimum) {
    failures.push(`${where}: minimum を下回ります`);
  }
  if (rules.maximum !== undefined && value > rules.maximum) {
    failures.push(`${where}: maximum を超えます`);
  }
  if (rules.uniqueItems && new Set(value.map(String)).size !== value.length) {
    failures.push(`${where}: 要素が重複しています`);
  }
  if (rules.maxItems !== undefined && value.length > rules.maxItems) {
    failures.push(`${where}: maxItems を超えます`);
  }
  if (rules.items !== undefined && Array.isArray(value)) {
    value.forEach((item, index) => failures.push(...validate(rules.items, item, `${where}[${index}]`)));
  }
  if (rules.propertyNames !== undefined) {
    for (const name of Object.keys(value)) {
      failures.push(...validate(rules.propertyNames, name, `${where}.${name} (名前)`));
    }
  }
  if (rules.required !== undefined) {
    for (const name of rules.required) {
      if (!(name in value)) failures.push(`${where}: ${name} がありません`);
    }
  }
  if (rules.properties !== undefined) {
    for (const [name, child] of Object.entries(rules.properties)) {
      if (name in value) failures.push(...validate(child, value[name], `${where}.${name}`));
    }
  }
  if (rules.additionalProperties === false && rules.properties !== undefined) {
    for (const name of Object.keys(value)) {
      if (!(name in rules.properties)) failures.push(`${where}.${name}: 未知の項目です`);
    }
  }
  if (typeof rules.additionalProperties === "object") {
    const declared = new Set(Object.keys(rules.properties ?? {}));
    for (const [name, child] of Object.entries(value)) {
      if (declared.has(name)) continue;
      failures.push(...validate(rules.additionalProperties, child, `${where}.${name}`));
    }
  }
  if (rules.not !== undefined && validate(rules.not, value, where).length === 0) {
    failures.push(`${where}: not に一致してしまいます`);
  }
  if (rules.oneOf !== undefined) {
    const matched = rules.oneOf.filter((child) => validate(child, value, where).length === 0);
    if (matched.length !== 1) {
      failures.push(`${where}: oneOf に一致する分岐が ${matched.length} 件です`);
    }
  }
  if (rules.allOf !== undefined) {
    for (const child of rules.allOf) failures.push(...validate(child, value, where));
  }
  if (rules.if !== undefined) {
    const matched = validate(rules.if, value, where).length === 0;
    if (matched && rules.then !== undefined) {
      failures.push(...validate(rules.then, value, where));
    }
  }
  return failures;
}

test("schemaはこのfileが評価できるkeywordだけを使う", () => {
  const unsupported = [...keywords(schema)].filter((keyword) => !SUPPORTED.has(keyword));
  assert.deepEqual(
    unsupported,
    [],
    `評価できないkeywordがあります。validatorを拡張してください: ${unsupported.join(", ")}`,
  );
});

test("JSON Schemaは実装と同じ設定を受理し、同じ設定を拒否する", () => {
  // 編集中の検証と実行時の検証が食い違うと、editorが通した設定を実行時が
  // 拒否します。同じcorpusを両方へ問い、食い違いを検出します。
  const disagreements = [];
  for (const entry of corpus.cases) {
    const failures = validate(schema, entry.config, entry.name);
    const schemaAccepts = failures.length === 0;
    if (schemaAccepts === entry.accepted) continue;
    if (!entry.accepted && schemaAccepts && entry.schemaOnlyAccepts) continue;
    disagreements.push(
      `${entry.name}: schema=${schemaAccepts ? "受理" : "拒否"} 実装=${entry.accepted ? "受理" : "拒否"}`,
    );
  }
  assert.deepEqual(disagreements, []);
});

test("文字列長はRustと同じUnicode scalar value数で検査する", () => {
  const rules = schema.$defs.workspaceScanPattern;
  assert.equal(validate(rules, "😀".repeat(1024), "境界値").length, 0);
  assert.notEqual(validate(rules, "😀".repeat(1025), "上限超過").length, 0);
});

test("JSON Schemaで表現できない制約には理由が書いてある", () => {
  for (const entry of corpus.cases.filter((entry) => entry.schemaOnlyAccepts)) {
    assert.equal(entry.accepted, false, entry.name);
    assert.ok(entry.reason, `${entry.name}: 表現できない理由がありません`);
    // 実装が拒否する設定をschemaが通すのは、表現できない場合だけです。
    assert.equal(validate(schema, entry.config, entry.name).length, 0, entry.name);
  }
});

/// Configuration fields the implementation resolves as a relative path.
///
/// The set is read from the implementation rather than listed here. A field
/// added there and forgotten in the schema is exactly what happened to
/// `html.stylesheet-files`: the schema constrained the two fields someone
/// remembered, and the corpus had no case for the third, so both sides agreed
/// while the schema accepted a path the run refuses.
function relativePathFields() {
  const source = readFileSync(
    new URL("../crates/adocweave-config/src/lib.rs", import.meta.url),
    "utf8",
  );
  const fields = new Set();
  for (const match of source.matchAll(/resolve_relative\(/g)) {
    const window = source.slice(match.index, match.index + 200);
    const quoted = window.match(/"([a-z][a-z0-9.-]*)(?:\.\{index\})?"/);
    if (quoted) fields.add(quoted[1]);
  }
  return [...fields].sort();
}

/// Walks a dotted configuration field to the schema node that constrains its value.
function schemaNodeFor(field) {
  let node = schema;
  for (const part of field.split(".")) {
    const properties = resolve(node).properties;
    if (!properties || !(part in properties)) return undefined;
    node = properties[part];
  }
  // An array field constrains each element rather than the array.
  const rules = resolve(node);
  return rules.type === "array" ? rules.items : node;
}

/// Reports whether a case's configuration writes a value for a dotted field.
function caseWrites(config, field) {
  let value = config;
  for (const part of field.split(".")) {
    if (value === null || typeof value !== "object" || !(part in value)) return false;
    value = value[part];
  }
  return true;
}

test("相対パスを解決する設定項目をschemaが制約している", () => {
  const fields = relativePathFields();
  assert.notEqual(fields.length, 0, "実装から相対パスの項目を読み取れません");

  const unconstrained = fields.filter((field) => {
    const node = schemaNodeFor(field);
    return node?.$ref !== "#/$defs/relativePath";
  });
  assert.deepEqual(
    unconstrained,
    [],
    `schemaが相対パスとして制約していない項目: ${unconstrained.join(", ")}`,
  );
});

test("相対パスを解決する設定項目をcorpusが受理と拒否の両方で覆っている", () => {
  // 制約を書いても、corpusに現れない項目は食い違いを検出できません。
  const missing = [];
  for (const field of relativePathFields()) {
    const written = corpus.cases.filter((entry) => caseWrites(entry.config, field));
    if (!written.some((entry) => entry.accepted)) missing.push(`${field}: 受理するcaseがありません`);
    if (!written.some((entry) => !entry.accepted)) missing.push(`${field}: 拒否するcaseがありません`);
  }
  assert.deepEqual(missing, []);
});

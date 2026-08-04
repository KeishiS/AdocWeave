const catalogKeys = ["forbiddenTerms", "schemaVersion"];
const entryKeys = ["documentation", "id", "match", "message", "term"];

function assertRecord(value, name) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${name}はオブジェクトで指定してください。`);
  }
}

function assertExactKeys(value, expected, name) {
  const actual = Object.keys(value).sort();
  if (actual.length !== expected.length || actual.some((key, index) => key !== expected[index])) {
    throw new Error(`${name}の項目がschemaと一致しません: ${actual.join(", ")}`);
  }
}

function assertNonEmptyString(value, name) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`${name}は空でない文字列で指定してください。`);
  }
}

function validateCatalog(catalog) {
  assertRecord(catalog, "日本語用語集");
  assertExactKeys(catalog, catalogKeys, "日本語用語集");
  if (catalog.schemaVersion !== 1 || !Array.isArray(catalog.forbiddenTerms)) {
    throw new Error("日本語用語集のschemaVersionを解釈できません。");
  }

  const ids = new Set();
  const entries = catalog.forbiddenTerms.map((entry, index) => {
    const name = `日本語用語集のforbiddenTerms[${index}]`;
    assertRecord(entry, name);
    assertExactKeys(entry, entryKeys, name);
    assertNonEmptyString(entry.id, `${name}.id`);
    assertNonEmptyString(entry.term, `${name}.term`);
    assertNonEmptyString(entry.message, `${name}.message`);
    assertNonEmptyString(entry.documentation, `${name}.documentation`);
    if (entry.match !== "substring") {
      throw new Error(`${name}.matchを解釈できません: ${String(entry.match)}`);
    }
    if (ids.has(entry.id)) {
      throw new Error(`日本語用語集のidが重複しています: ${entry.id}`);
    }
    ids.add(entry.id);
    return Object.freeze({
      id: entry.id,
      term: entry.term,
      match: entry.match,
      message: entry.message,
      documentation: entry.documentation
    });
  });
  return Object.freeze(entries);
}

export function createTerminologyRule(catalog) {
  const entries = validateCatalog(catalog);
  return (context) => {
    const { Syntax, RuleError, locator, report } = context;
    return {
      [Syntax.Str](node) {
        for (const entry of entries) {
          let start = 0;
          while (start <= node.value.length) {
            const index = node.value.indexOf(entry.term, start);
            if (index === -1) {
              break;
            }
            report(
              node,
              new RuleError(`${entry.message} [${entry.id}]`, {
                padding: locator.range([index, index + entry.term.length])
              })
            );
            start = index + entry.term.length;
          }
        }
      }
    };
  };
}

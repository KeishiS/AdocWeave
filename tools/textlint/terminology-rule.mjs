export function createTerminologyRule(catalog) {
  if (catalog.schemaVersion !== 1 || !Array.isArray(catalog.forbiddenTerms)) {
    throw new Error("日本語用語集のschemaVersionを解釈できません。");
  }
  return (context) => {
    const { Syntax, RuleError, locator, report } = context;
    return {
      [Syntax.Str](node) {
        for (const entry of catalog.forbiddenTerms) {
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

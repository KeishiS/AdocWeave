import assert from "node:assert/strict";
import test from "node:test";

import { TextlintKernel } from "@textlint/kernel";
import commentsFilter from "textlint-filter-rule-comments";

import plugin from "./processor.mjs";
import { createTerminologyRule } from "./terminology-rule.mjs";

const catalog = {
  schemaVersion: 1,
  forbiddenTerms: [
    {
      id: "sample",
      term: "禁止語",
      message: "別の表現を検討してください。"
    }
  ]
};

async function lint(source) {
  return new TextlintKernel().lintText(source, {
    ext: ".adoc",
    filePath: "test.adoc",
    plugins: [{ pluginId: "adocweave", plugin }],
    rules: [
      {
        ruleId: "adocweave-terminology",
        rule: createTerminologyRule(catalog)
      }
    ],
    filterRules: [{ ruleId: "comments", rule: commentsFilter }]
  });
}

test("地の文にある禁止語の元位置を報告する", async () => {
  const result = await lint("= 文書\n\n😀禁止語です。\n");
  assert.equal(result.messages.length, 1);
  assert.equal(result.messages[0].line, 3);
  assert.equal(result.messages[0].column, 3);
  assert.match(result.messages[0].message, /\[sample\]/);
});

test("inline codeとsource blockを検査しない", async () => {
  const result = await lint(
    "= 文書\n\n`禁止語` です。\n\n[source,text]\n----\n禁止語\n----\n"
  );
  assert.equal(result.messages.length, 0);
});

test("AsciiDocコメントによる局所的な抑制を適用する", async () => {
  const result = await lint(
    "= 文書\n\n// textlint-disable adocweave-terminology\n禁止語です。\n// textlint-enable adocweave-terminology\n\n禁止語です。\n"
  );
  assert.equal(result.messages.length, 1);
  assert.equal(result.messages[0].line, 7);
});

import assert from "node:assert/strict";
import test from "node:test";

import plugin, { Processor } from "./index.mjs";
import { createProcessorClass } from "./processor.mjs";

function fixture() {
  const source = "= 見出し\n\n* 項目\n\nlink:https://example.com[表示]\n\n[source,rust]\n----\nlet x = 1;\n----\n";
  const range = (text, from = 0) => {
    const start = source.indexOf(text, from);
    return [start, start + text.length];
  };
  const headingRange = range("= 見出し");
  const headingTextRange = range("見出し");
  const listRange = range("* 項目");
  const itemTextRange = range("項目");
  const linkSource = "link:https://example.com[表示]";
  const linkRange = range(linkSource);
  const linkTextRange = range("表示", source.indexOf(linkSource));
  const blockSource = "[source,rust]\n----\nlet x = 1;\n----";
  const blockRange = range(blockSource);
  const codeRange = range("let x = 1;");
  return {
    source,
    plan: {
      type: "Document",
      range: [0, source.length],
      children: [
        {
          type: "Header",
          range: headingRange,
          depth: 1,
          children: [{ type: "Str", range: headingTextRange, valueRange: headingTextRange }]
        },
        {
          type: "List",
          range: listRange,
          ordered: false,
          children: [{
            type: "ListItem",
            range: listRange,
            children: [{
              type: "Paragraph",
              range: itemTextRange,
              children: [{ type: "Str", range: itemTextRange, valueRange: itemTextRange }]
            }]
          }]
        },
        {
          type: "Paragraph",
          range: linkRange,
          children: [{
            type: "Link",
            range: linkRange,
            url: "https://example.com",
            children: [{ type: "Str", range: linkTextRange, valueRange: linkTextRange }]
          }]
        },
        { type: "CodeBlock", range: blockRange, valueRange: codeRange, lang: "rust" }
      ]
    }
  };
}

function processorFor(plan) {
  return new (createProcessorClass(() => plan))();
}

function descendants(root) {
  const nodes = [];
  const visit = (node) => {
    nodes.push(node);
    for (const child of node.children ?? []) visit(child);
  };
  visit(root);
  return nodes;
}

test("default exportから公開Processorだけを公開する", () => {
  assert.equal(plugin.Processor, Processor);
  assert.throws(() => new Processor({}, { parseText() {} }), /optionsだけ/);
});

test("追加拡張子を検証して重複なく登録する", () => {
  const InjectedProcessor = createProcessorClass(() => fixture().plan);
  const processor = new InjectedProcessor({ extensions: [".guide", ".ADOC", ".guide"] });
  assert.deepEqual(processor.availableExtensions(), [".adoc", ".asciidoc", ".asc", ".guide"]);
  assert.doesNotThrow(() => processor.processor(".GUIDE"));
  assert.throws(() => new InjectedProcessor({ extensions: ["guide"] }), /形式が不正/);
  assert.throws(() => new InjectedProcessor({ extensions: "guide" }), /配列/);
  assert.throws(() => processor.processor(".md"), /未対応/);
});

test("planをkind別処理なしでTxtASTへ具体化する", () => {
  const { source, plan } = fixture();
  let request;
  const InjectedProcessor = createProcessorClass((input, filePath) => {
    request = { input, filePath };
    return plan;
  });
  const ast = new InjectedProcessor().processor(".adoc").preProcess(source, "文書.adoc");
  assert.deepEqual(request, { input: source, filePath: "文書.adoc" });
  for (const node of descendants(ast)) {
    assert.equal(node.raw, source.slice(node.range[0], node.range[1]), node.type);
    for (const child of node.children ?? []) {
      assert.ok(node.range[0] <= child.range[0], `${node.type} start`);
      assert.ok(child.range[1] <= node.range[1], `${node.type} end`);
    }
  }
  const nodes = descendants(ast);
  assert.equal(nodes.find((node) => node.type === "Header").depth, 1);
  assert.equal(nodes.find((node) => node.type === "List").ordered, false);
  assert.equal(nodes.find((node) => node.type === "Link").url, "https://example.com");
  assert.equal(nodes.find((node) => node.type === "CodeBlock").lang, "rust");
  assert.equal(nodes.find((node) => node.type === "CodeBlock").value, "let x = 1;");
  assert.equal(nodes.find((node) => node.type === "Str").value, "見出し");
});

test("任意のnode固有propertyを保持し、生成propertyの上書きを拒ぐ", () => {
  const source = "本文";
  const plan = {
    type: "Document",
    range: [0, source.length],
    raw: "偽の原文",
    loc: null,
    children: [{
      type: "FutureNode",
      range: [0, source.length],
      valueRange: [0, source.length],
      futureProperty: { enabled: true },
      value: "偽の値"
    }]
  };
  const ast = processorFor(plan).processor(".adoc").preProcess(source);
  assert.equal(ast.raw, source);
  assert.equal(ast.children[0].value, source);
  assert.deepEqual(ast.children[0].futureProperty, { enabled: true });
});

test("planの構造とUTF-16 rangeをfail closedで検証する", () => {
  const cases = [
    [{ type: "Paragraph", range: [0, 0], children: [] }, /root/],
    [{ type: "Document", range: [0, 1], children: [] }, /不正/],
    [{ type: "Document", range: [0, 0] }, /children/],
    [{ type: "Document", range: [0, 0], children: {} }, /children/]
  ];
  for (const [plan, expected] of cases) {
    assert.throws(() => processorFor(plan).processor(".adoc").preProcess(""), expected);
  }

  const { source, plan } = fixture();
  plan.children[0].children[0].range = plan.range;
  assert.throws(
    () => processorFor(plan).processor(".adoc").preProcess(source),
    /親nodeのrange/
  );
});

test("valueRangeをnodeのrange内に限定する", () => {
  const source = "本文";
  const plan = {
    type: "Document",
    range: [0, source.length],
    children: [{ type: "Str", range: [1, 2], valueRange: [0, 2] }]
  };
  assert.throws(
    () => processorFor(plan).processor(".adoc").preProcess(source),
    /valueRangeがrangeに含まれていません/
  );
});

test("兄弟nodeのrange重複を拒否する", () => {
  const source = "本文です";
  const plan = {
    type: "Document",
    range: [0, source.length],
    children: [
      { type: "Str", range: [0, 3], valueRange: [0, 3] },
      { type: "Str", range: [2, source.length], valueRange: [2, source.length] }
    ]
  };
  assert.throws(
    () => processorFor(plan).processor(".adoc").preProcess(source),
    /重複/
  );
});

test("表の親子関係をplanどおり保持する", () => {
  const source = "|===\n|セル\n|===\n";
  const cellStart = source.indexOf("セル");
  const cellRange = [cellStart, cellStart + "セル".length];
  const plan = {
    type: "Document",
    range: [0, source.length],
    children: [{
      type: "Table",
      range: [0, source.length - 1],
      children: [{
        type: "TableRow",
        range: cellRange,
        children: [{
          type: "TableCell",
          range: cellRange,
          children: [{ type: "Str", range: cellRange, valueRange: cellRange }]
        }]
      }]
    }]
  };
  const [table] = processorFor(plan).processor(".adoc").preProcess(source).children;
  assert.equal(table.type, "Table");
  assert.equal(table.children[0].type, "TableRow");
  assert.equal(table.children[0].children[0].type, "TableCell");
  assert.equal(table.children[0].children[0].children[0].value, "セル");
});

test("postProcessは入力を変えずにfixだけを除去する", () => {
  const original = [{ ruleId: "example", message: "問題です。", fix: { range: [0, 1], text: "修正" } }];
  const output = processorFor({}).processor(".adoc").postProcess(original, undefined);
  assert.deepEqual(output, {
    messages: [{ ruleId: "example", message: "問題です。" }],
    filePath: "<text>"
  });
  assert.ok("fix" in original[0]);
});

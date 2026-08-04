import assert from "node:assert/strict";
import test from "node:test";

import plugin, { Processor } from "./index.mjs";

function fixture() {
  const source = "= 見出し\n\n* 項目\n\nlink:https://example.com[表示]\n\n[source,rust]\n----\nlet x = 1;\n----\n";
  const bytes = (offset) => Buffer.byteLength(source.slice(0, offset));
  const range = (text, from = 0) => {
    const start = source.indexOf(text, from);
    return [bytes(start), bytes(start + text.length)];
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
  const node = (kind, sourceRange, contentRange, children = [], extra = {}) => ({
    kind,
    sourceRange,
    contentRange,
    children,
    ...extra
  });
  return {
    source,
    projection: {
      sourceRange: [0, Buffer.byteLength(source)],
      children: [
        node("heading", headingRange, headingTextRange, [
          node("text", headingTextRange, headingTextRange)
        ], { level: 1 }),
        node("list", listRange, listRange, [
          node("list-item", listRange, itemTextRange, [
            node("text", itemTextRange, itemTextRange)
          ])
        ], { ordered: false }),
        node("paragraph", linkRange, linkRange, [
          node("link", linkRange, linkTextRange, [
            node("text", linkTextRange, linkTextRange)
          ], { url: "https://example.com" })
        ]),
        node("code-block", blockRange, codeRange, [], { language: "rust" })
      ]
    }
  };
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

test("default exportからProcessorを公開する", () => {
  assert.equal(plugin.Processor, Processor);
});

test("追加拡張子を検証して重複なく登録する", () => {
  const processor = new Processor({ extensions: [".guide", ".ADOC", ".guide"] });
  assert.deepEqual(processor.availableExtensions(), [".adoc", ".asciidoc", ".asc", ".guide"]);
  assert.doesNotThrow(() => processor.processor(".GUIDE"));
  assert.throws(() => new Processor({ extensions: ["guide"] }), /形式が不正/);
  assert.throws(() => new Processor({ extensions: "guide" }), /配列/);
  assert.throws(() => processor.processor(".md"), /未対応/);
});

test("node固有propertyと全range不変条件を維持する", () => {
  const { source, projection } = fixture();
  let request;
  const processor = new Processor({}, {
    projectText(input, filePath) {
      request = { input, filePath };
      return projection;
    }
  }).processor(".adoc");
  const ast = processor.preProcess(source, "文書.adoc");
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
});

test("projectionがnode固有propertyを欠く場合は値を捏造しない", () => {
  for (const [index, property] of [[1, "ordered"], [2, "url"], [3, "language"]]) {
    const { source, projection } = fixture();
    delete projection.children[index][property];
    if (property === "url") delete projection.children[index].children[0].url;
    const processor = new Processor({}, { projectText: () => projection }).processor(".adoc");
    assert.throws(() => processor.preProcess(source), new RegExp(property));
  }
});

test("親range外のnodeを拒否する", () => {
  const { source, projection } = fixture();
  projection.children[0].children[0].sourceRange = projection.sourceRange;
  const processor = new Processor({}, { projectText: () => projection }).processor(".adoc");
  assert.throws(() => processor.preProcess(source), /親nodeのrange/);
});

test("見出しlevelとcode blockのcontentRangeを必須とする", () => {
  for (const [index, property] of [[0, "level"], [3, "contentRange"]]) {
    const { source, projection } = fixture();
    delete projection.children[index][property];
    const processor = new Processor({}, { projectText: () => projection }).processor(".adoc");
    assert.throws(() => processor.preProcess(source), new RegExp(property));
  }
});

test("postProcessは入力を変えずにfixだけを除去する", () => {
  const original = [{ ruleId: "example", message: "問題です。", fix: { range: [0, 1], text: "修正" } }];
  const output = new Processor({}, { projectText: () => ({}) })
    .processor(".adoc")
    .postProcess(original, undefined);
  assert.deepEqual(output, {
    messages: [{ ruleId: "example", message: "問題です。" }],
    filePath: "<text>"
  });
  assert.ok("fix" in original[0]);
});

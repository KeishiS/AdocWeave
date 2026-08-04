import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { test as testAST } from "@textlint/ast-tester";

import { Processor } from "./processor.mjs";
import { classifyTrackedFiles } from "./repository-lint-config.mjs";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const targets = JSON.parse(readFileSync(new URL("./targets.json", import.meta.url), "utf8"));

test("AsciiDocを有効なTxtASTへ変換する", () => {
  const source = "= 文書\n\n// textlint-disable\n\n== 節\n\n本文の **強調** と `code` です。\n\n* 項目\n";
  const ast = new Processor().processor(".adoc").preProcess(source, "test.adoc");
  testAST(ast);
  assert.equal(ast.type, "Document");
  assert.equal(ast.raw, source);
  assert.ok(ast.children.some((node) => node.type === "Header"));
  assert.ok(ast.children.some((node) => node.type === "List"));
});

test("TxtAST固有のプロパティを保持する", () => {
  const source = `:site: https://example.com
:page: other

link:{site}[表示] と xref:{page}.adoc#section[参照]

* 箇条書き

. 番号付き

----
plain
----

[source,rust]
----
fn main() {}
----
`;
  const ast = new Processor().processor(".adoc").preProcess(source, "properties.adoc");
  testAST(ast);

  const nodes = [];
  const stack = [ast];
  while (stack.length > 0) {
    const node = stack.pop();
    nodes.push(node);
    stack.push(...(node.children ?? []));
  }

  const links = nodes.filter((node) => node.type === "Link");
  assert.deepEqual(
    links.map((node) => node.url),
    ["other.adoc#section", "https://example.com"]
  );
  assert.deepEqual(
    links.map((node) => node.children.map((child) => child.value).join("")),
    ["参照", "表示"]
  );
  assert.deepEqual(
    nodes.filter((node) => node.type === "List").map((node) => node.ordered),
    [true, false]
  );
  assert.deepEqual(
    nodes.filter((node) => node.type === "CodeBlock").map((node) => node.lang),
    ["rust", null]
  );
});

test("未対応の拡張子を拒否する", () => {
  assert.throws(() => new Processor().processor(".md"), /未対応/);
});

test("属性参照、pass、URL、includeおよび未対応構文を文章規則へ渡さない", () => {
  const source = `:name: 属性参照の値

本文 {name} pass:[インライン通過] https://example.invalid/path

include::存在しないpart.adoc[]

++++
ブロック通過
++++

[source,rust,options=unknown]
----
unsupported_marker();
----
`;
  const ast = new Processor().processor(".adoc").preProcess(source, "excluded.adoc");
  testAST(ast);
  const nodes = [];
  const stack = [ast];
  while (stack.length > 0) {
    const node = stack.pop();
    nodes.push(node);
    stack.push(...(node.children ?? []));
  }
  const prose = nodes
    .filter((node) => node.type === "Str")
    .map((node) => node.value)
    .join("");
  assert.match(prose, /本文/);
  for (const excluded of [
    "属性参照の値",
    "インライン通過",
    "example.invalid",
    "存在しないpart.adoc",
    "ブロック通過",
    "unsupported_marker",
  ]) {
    assert.ok(!prose.includes(excluded), `${excluded}が文章規則へ渡されました`);
  }
  const link = nodes.find((node) => node.type === "Link");
  assert.equal(link.url, "https://example.invalid/path");
  assert.deepEqual(link.children, []);
});

test("すべての執筆文書を原文に対応するTxtASTへ変換する", () => {
  const tracked = execFileSync("git", ["ls-files", "*.adoc"], {
    cwd: repositoryRoot,
    encoding: "utf8"
  })
    .trim()
    .split("\n")
    .filter(Boolean);
  const authored = classifyTrackedFiles(targets, tracked).authored;
  assert.notEqual(authored.length, 0);
  const processor = new Processor().processor(".adoc");
  for (const path of authored) {
    const source = readFileSync(`${repositoryRoot}${path}`, "utf8");
    const ast = processor.preProcess(source, path);
    testAST(ast);
    const stack = [ast];
    while (stack.length > 0) {
      const node = stack.pop();
      assert.equal(node.raw, source.slice(node.range[0], node.range[1]), `${path}: ${node.type}`);
      if (node.children) {
        stack.push(...node.children);
      }
    }
  }
});

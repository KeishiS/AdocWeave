import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { test as testAST } from "@textlint/ast-tester";
import { TextlintKernel } from "@textlint/kernel";
import technicalWriting from "textlint-rule-preset-ja-technical-writing";

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

test("日本語技術文書presetがブロックタイトルと文末の脚注へ誤警告しない", async () => {
  const source = ".表題\n本文です。 footnote:[注釈です。]\n";
  const result = await new TextlintKernel().lintText(source, {
    ext: ".adoc",
    filePath: "block-title.adoc",
    plugins: [{ pluginId: "adocweave", plugin: { Processor } }],
    rules: Object.entries(technicalWriting.rules).map(([ruleId, rule]) => ({
      ruleId,
      rule,
      options: structuredClone(technicalWriting.rulesConfig[ruleId])
    }))
  });
  assert.deepEqual(result.messages, []);
});

test("macroの自然文だけをStrとして公開する", () => {
  const source = `本文です。 footnote:[注釈です。] footnote:[C++ APIです。]

kbd:[Ctrl,Shift,T] btn:[保存] menu:File[Open,Recent]

image:pic.png[代替文] icon:save[title=アイコン説明]

audio:sound.mp3[] video:movie.mp4[] icon:save[]

btn:[{label}] image:pic.png[{alt}] footnote:[**強調**です。]

footnote:[https://example.com/path] footnote:[user@example.com]

image:pic.png[https://example.com] audio:x.mp3[title=https://example.com]
`;
  const ast = new Processor().processor(".adoc").preProcess(source, "macros.adoc");
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
    .map((node) => node.value);
  for (const expected of ["注釈です。", "C++ APIです。", "代替文", "アイコン説明"]) {
    assert.ok(prose.includes(expected), `${expected}が文章として公開されていません`);
  }
  for (const excluded of [
    "sound.mp3",
    "movie.mp4",
    "save",
    "{label}",
    "{alt}",
    "**強調**です。",
    "https://example.com/path",
    "user@example.com",
    "https://example.com"
  ]) {
    assert.ok(!prose.includes(excluded), `${excluded}が文章として公開されました`);
  }
  const code = nodes
    .filter((node) => node.type === "Code")
    .map((node) => node.value);
  for (const expected of ["Ctrl", "Shift", "T", "保存", "File", "Open", "Recent"]) {
    assert.ok(code.includes(expected), `${expected}がUI tokenとして保持されていません`);
  }
});

test("対象外inlineが見出しと表の構造を分断しない", () => {
  const source = "== 前 {unknown} 後\n\n|===\n|前{unknown}後\n|===\n";
  const ast = new Processor().processor(".adoc").preProcess(source, "opaque.adoc");
  testAST(ast);
  assert.equal(ast.children.filter((node) => node.type === "Header").length, 1);
  const cells = [];
  const stack = [ast];
  while (stack.length > 0) {
    const node = stack.pop();
    if (node.type === "TableCell") cells.push(node);
    stack.push(...(node.children ?? []));
  }
  assert.equal(cells.length, 1);
  assert.ok(cells[0].children.some((node) => node.type === "Code"));
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
  assert.ok(
    !nodes.some((node) => node.type === "Link"),
    "表示文字列のないURLをLink nodeとして渡しました"
  );
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

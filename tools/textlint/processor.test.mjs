import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { test as testAST } from "@textlint/ast-tester";

import { Processor } from "./processor.mjs";

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

test("未対応の拡張子を拒否する", () => {
  assert.throws(() => new Processor().processor(".md"), /未対応/);
});

test("すべての執筆文書を原文に対応するTxtASTへ変換する", () => {
  const tracked = execFileSync("git", ["ls-files", "*.adoc"], {
    cwd: repositoryRoot,
    encoding: "utf8"
  })
    .trim()
    .split("\n")
    .filter(Boolean);
  const authored = tracked.filter(
    (path) =>
      targets.authoredFiles.includes(path) ||
      targets.authoredDirectories.some((directory) => path.startsWith(directory))
  );
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

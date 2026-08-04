import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { createProjectText } from "./bridge.mjs";

const packageVersion = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8")
).version;

test("package versionとsourceをWASMへ渡す", () => {
  let request;
  const projectText = createProjectText(() => ({
    projectText(value) {
      request = value;
      return { sourceRange: [0, 0], children: [] };
    }
  }));
  assert.deepEqual(projectText("", "doc.adoc"), { sourceRange: [0, 0], children: [] });
  assert.deepEqual(request, {
    packageVersion,
    sourceId: "doc.adoc",
    source: ""
  });
});

test("WASMのJSON errorをcode付きErrorへ変換する", () => {
  for (const code of [
    "unsupported-api-version",
    "input-too-large",
    "output-too-large",
    "node-limit",
    "invalid-request"
  ]) {
    const projectText = createProjectText(() => ({
      projectText() {
        throw JSON.stringify({ code, message: `${code}の説明` });
      }
    }));
    assert.throws(
      () => projectText(""),
      (error) => error instanceof Error && error.code === code && error.message === `${code}の説明`
    );
  }
});

test("WASM初期化失敗を利用者向けErrorへ変換する", () => {
  const initializationError = new Error("同梱されたAdocWeave WebAssemblyを読み込めませんでした。");
  initializationError.code = "wasm-initialization-failed";
  const projectText = createProjectText(() => {
    throw initializationError;
  });
  assert.throws(
    () => projectText(""),
    (error) =>
      error instanceof Error &&
      error.code === "wasm-initialization-failed" &&
      error.message.includes("WebAssembly")
  );
});

test("未知のthrow値もErrorへ変換する", () => {
  const projectText = createProjectText(() => ({
    projectText() {
      throw 42;
    }
  }));
  assert.throws(
    () => projectText(""),
    (error) => error instanceof Error && error.code === "adocweave-error"
  );
});

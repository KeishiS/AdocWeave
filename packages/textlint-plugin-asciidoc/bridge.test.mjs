import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { createParseText } from "./bridge.mjs";

const componentVersion = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8")
).version;

test("component version、sourceIdおよびsourceをWASMへ渡す", () => {
  let request;
  const parseText = createParseText({
    componentVersion,
    bridgeLoader: () => ({
      parseText(value) {
        request = value;
        return { type: "Document", range: [0, 0], children: [] };
      }
    })
  });
  assert.deepEqual(parseText("", "doc.adoc"), {
    type: "Document",
    range: [0, 0],
    children: []
  });
  assert.deepEqual(request, {
    componentVersion,
    sourceId: "doc.adoc",
    source: ""
  });
});

test("WASMのJSON errorをcode付きErrorへ変換する", () => {
  for (const code of [
    "component-version-mismatch",
    "input-too-large",
    "output-too-large",
    "node-limit",
    "invalid-request"
  ]) {
    const parseText = createParseText({
      componentVersion,
      bridgeLoader: () => ({
        parseText() {
          throw JSON.stringify({ code, message: `${code}の説明` });
        }
      })
    });
    assert.throws(
      () => parseText(""),
      (error) => error instanceof Error && error.code === code && error.message === `${code}の説明`
    );
  }
});

test("factoryが設定、requestおよびWASM exportを検証する", () => {
  assert.throws(() => createParseText({ componentVersion, bridgeLoader: null }), /bridgeLoader/);
  assert.throws(
    () => createParseText({ componentVersion: "", bridgeLoader: () => ({}) }),
    /componentVersion/
  );
  const missingExport = createParseText({ componentVersion, bridgeLoader: () => ({}) });
  assert.throws(
    () => missingExport(""),
    (error) => error.code === "wasm-initialization-failed" && /parseText/.test(error.message)
  );
  const parseText = createParseText({
    componentVersion,
    bridgeLoader: () => ({ parseText: () => ({}) })
  });
  assert.throws(() => parseText(new Uint8Array()), /文字列/);
  assert.throws(() => parseText("", 42), /sourceId/);
});

test("初期化失敗と未知のthrow値を利用者向けErrorへ変換する", () => {
  const initializationFailure = createParseText({
    componentVersion,
    bridgeLoader: () => {
      throw new Error("module load failed");
    }
  });
  assert.throws(
    () => initializationFailure(""),
    (error) =>
      error.code === "wasm-initialization-failed" &&
      /WebAssembly/.test(error.message) &&
      error.cause?.message === "module load failed"
  );

  const unknownFailure = createParseText({
    componentVersion,
    bridgeLoader: () => ({ parseText: () => { throw 42; } })
  });
  assert.throws(
    () => unknownFailure(""),
    (error) => error instanceof Error && error.code === "adocweave-error"
  );
});

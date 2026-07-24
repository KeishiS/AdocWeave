import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const fixtures = resolve(root, "fixtures/conformance");
const manifest = JSON.parse(readFileSync(resolve(fixtures, "cases.json"), "utf8"));
const require = createRequire(import.meta.url);
const wasm = require(resolve(root, "target/adocweave-wasm-node/adocweave_wasm.js"));
const native = resolve(root, "target/debug/adocweave-conformance-native");
const release = JSON.parse(readFileSync(resolve(root, "release-manifest.json"), "utf8"));

function requestFor(entry) {
  const source = entry.sourceFile
    ? readFileSync(resolve(fixtures, entry.sourceFile), "utf8")
    : entry.source;
  return {
    packageVersion: release.packageVersion,
    sourceId: `conformance:${entry.name}`,
    version: 1,
    generation: 1,
    source,
    products: {
      syntax: true,
      canonicalAst: true,
      html: true,
      attributeOccurrences: true,
      diagnostics: true,
      symbols: true,
      projection: true,
    },
    renderInputs: entry.renderInputs ?? {},
    options: entry.options ?? {},
  };
}

function nativeResult(request) {
  const run = spawnSync(native, [], {
    cwd: root,
    input: `${JSON.stringify(request)}\n`,
    encoding: "utf8",
  });
  assert.equal(run.status, 0, run.stderr);
  return JSON.parse(run.stdout);
}

function wasmResult(request) {
  try {
    return { ok: true, value: wasm.process(request) };
  } catch (error) {
    const text = String(error);
    let value;
    try {
      value = JSON.parse(text);
    } catch {
      value = text;
    }
    return { ok: false, error: value };
  }
}

for (const entry of manifest.cases) {
  test(`native and WASM agree: ${entry.name}`, () => {
    const request = requestFor(entry);
    const expected = nativeResult(request);
    const actual = wasmResult(request);
    assert.deepEqual(actual, expected);

    if (entry.expectedHtmlFile) {
      assert.equal(
        actual.value.html,
        readFileSync(resolve(fixtures, entry.expectedHtmlFile), "utf8"),
      );
    }
    if (entry.expectedAstFile) {
      assert.equal(
        actual.value.ast,
        readFileSync(resolve(fixtures, entry.expectedAstFile), "utf8").trimEnd(),
      );
    }
    for (const [field, product] of [
      ["expectedDiagnosticsFile", "diagnostics"],
      ["expectedProjectionFile", "projection"],
      ["expectedSymbolsFile", "symbols"],
    ]) {
      if (entry[field]) {
        assert.deepEqual(
          actual.value[product],
          JSON.parse(readFileSync(resolve(fixtures, entry[field]), "utf8")),
        );
      }
    }
    if (entry.expectedErrorCode) {
      assert.equal(actual.error.code, entry.expectedErrorCode);
    }
  });
}

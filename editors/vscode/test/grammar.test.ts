import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";

import { loadWASM, OnigScanner, OnigString } from "vscode-oniguruma";
import { Registry, type IGrammar } from "vscode-textmate";

interface ScopeFixture {
  readonly scope: string;
  readonly source: string;
}

async function loadGrammar(): Promise<IGrammar> {
  const wasm = await readFile(require.resolve("vscode-oniguruma/release/onig.wasm"));
  await loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const grammarSource = await readFile(
    join(__dirname, "..", "syntaxes", "asciidoc.tmLanguage.json"),
    "utf8",
  );
  const registry = new Registry({
    loadGrammar: async (scopeName) =>
      scopeName === "text.asciidoc" ? JSON.parse(grammarSource) : null,
    onigLib: Promise.resolve({
      createOnigScanner: (sources) => new OnigScanner(sources),
      createOnigString: (source) => new OnigString(source),
    }),
  });
  const grammar = await registry.loadGrammar("text.asciidoc");
  assert.ok(grammar);
  return grammar;
}

test("TextMate grammarは代表的なAsciiDoc字句へ安定したscopeを付与します", async () => {
  const fixtures = JSON.parse(
    await readFile(join(__dirname, "fixtures", "grammar-scopes.json"), "utf8"),
  ) as ScopeFixture[];
  const grammar = await loadGrammar();
  for (const fixture of fixtures) {
    const scopes = grammar
      .tokenizeLine(fixture.source, null)
      .tokens.flatMap((token) => token.scopes);
    assert.ok(
      scopes.includes(fixture.scope),
      `${JSON.stringify(fixture.source)}に${fixture.scope}がありません：${scopes.join(", ")}`,
    );
  }
});

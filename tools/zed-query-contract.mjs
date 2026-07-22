import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");
const snapshot = JSON.parse(read("tools/zed-query-nodes.json"));
const manifest = read("editors/zed/extension.toml");
const inlineConfig = read("editors/zed/languages/asciidoc_inline/config.toml");

function fail(message) {
  throw new Error(message);
}

function grammarSection(name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = manifest.match(new RegExp(`\\[grammars\\.${escaped}\\]([\\s\\S]*?)(?=\\n\\[|$)`));
  if (!match) fail(`missing grammar declaration: ${name}`);
  return match[1];
}

function queryNodes(source) {
  return [...source.matchAll(/\(([a-z][a-z0-9_]*)\b/g)].map((match) => match[1]);
}

if (!/^name = "AsciiDoc Inline"$/m.test(inlineConfig) || !/^hidden = true$/m.test(inlineConfig)) {
  fail("AsciiDoc inline language must be registered by its injection name and hidden from users");
}

const mainInjections = read("editors/zed/languages/asciidoc/injections.scm");
if (!mainInjections.includes('(#set! injection.language "AsciiDoc Inline")') ||
    mainInjections.includes('(#set! injection.language "asciidoc_inline")')) {
  fail("AsciiDoc inline injections must use the registered language name, not the grammar id");
}

const highlightCaptures = new Set([
  "attribute", "comment", "constant", "emphasis", "emphasis.strong", "keyword", "label",
  "link_text", "link_uri", "number", "property", "punctuation.bracket",
  "punctuation.delimiter", "punctuation.list_marker", "punctuation.special", "string",
  "string.escape", "string.special", "text.literal", "title", "type", "variable.parameter",
]);

for (const [grammar, nodes] of Object.entries(snapshot.grammars)) {
  const section = grammarSection(grammar);
  if (!section.includes(`commit = "${snapshot.commit}"`)) {
    fail(`${grammar} does not use the node snapshot commit`);
  }
  if (!section.includes(`path = "tree-sitter-${grammar}"`)) {
    fail(`${grammar} uses an unexpected repository path`);
  }

  const known = new Set(nodes);
  for (const query of ["highlights.scm", "injections.scm"]) {
    const path = `editors/zed/languages/${grammar}/${query}`;
    const source = read(path);
    for (const node of queryNodes(source)) {
      if (!known.has(node)) fail(`${path} references unknown ${grammar} node: ${node}`);
    }
    const captures = [...source.matchAll(/@([A-Za-z_][A-Za-z0-9_.-]*)/g)].map((match) => match[1]);
    if (query === "injections.scm" && captures.includes("content") && captures.includes("injection.content")) {
      fail(`${path} mixes @content and @injection.content`);
    }
    for (const capture of captures) {
      if (capture.startsWith("_")) continue;
      if (query === "injections.scm") {
        if (!["content", "injection.content", "injection.language"].includes(capture)) {
          fail(`${path} uses unsupported injection capture: @${capture}`);
        }
      } else if (!highlightCaptures.has(capture)) {
        fail(`${path} uses unsupported highlight capture: @${capture}`);
      }
    }
  }
}

process.stdout.write(`Zed query contract verified: ${snapshot.commit}\n`);

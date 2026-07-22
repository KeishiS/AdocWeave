import { readFileSync } from "node:fs";
import process from "node:process";

const ROOT = new URL("../", import.meta.url);
const read = (path) => readFileSync(new URL(path, ROOT), "utf8");
const snapshot = JSON.parse(read("tools/zed-query-nodes.json"));
const manifest = read("editors/zed/extension.toml");

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

const highlightCaptures = new Set([
  "attribute", "comment", "comment.error", "comment.note", "comment.warning", "constant", "keyword",
  "label", "markup.heading.1", "markup.heading.2", "markup.heading.3", "markup.heading.4",
  "markup.heading.5", "markup.heading.6", "markup.italic", "markup.link.label", "markup.link.url",
  "markup.list", "markup.list.checked", "markup.list.unchecked", "markup.raw", "markup.raw.block",
  "markup.strong", "markup.subscript", "markup.superscript", "number", "property",
  "punctuation.bracket", "punctuation.delimiter", "punctuation.special", "string", "string.escape",
  "string.special", "type", "variable.parameter",
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

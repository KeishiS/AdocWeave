import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { TextlintKernel } from "@textlint/kernel";
import commentsFilter from "textlint-filter-rule-comments";
import technicalWriting from "textlint-rule-preset-ja-technical-writing";

import plugin from "./processor.mjs";
import { createTerminologyRule } from "./terminology-rule.mjs";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const targets = JSON.parse(readFileSync(new URL("./targets.json", import.meta.url), "utf8"));
const terminology = JSON.parse(
  readFileSync(new URL("../../config/japanese-terminology.json", import.meta.url), "utf8")
);

if (targets.schemaVersion !== 1) {
  throw new Error("文書対象一覧のschemaVersionを解釈できません。");
}

const tracked = execFileSync("git", ["ls-files", "-z", "*.adoc"], {
  cwd: repositoryRoot,
  encoding: "utf8"
})
  .split("\0")
  .filter(Boolean);

function classification(path) {
  if (targets.authoredFiles.includes(path)) {
    return "authored";
  }
  if (targets.authoredDirectories.some((directory) => path.startsWith(directory))) {
    return "authored";
  }
  if (targets.excludedDirectories.some((entry) => path.startsWith(entry.path))) {
    return "excluded";
  }
  return "unknown";
}

const unknown = tracked.filter((path) => classification(path) === "unknown");
if (unknown.length > 0) {
  console.error(`校正対象が分類されていません。\n${unknown.join("\n")}`);
  process.exitCode = 2;
} else {
  const selectedRules = [
    "no-mix-dearu-desumasu",
    "no-double-negative-ja",
    "no-dropping-the-ra",
    "no-nfd",
    "no-hankaku-kana",
    "no-invalid-control-character",
    "no-unmatched-pair",
    "no-zero-width-spaces"
  ];
  const rules = selectedRules.map((ruleId) => ({
    ruleId,
    rule: technicalWriting.rules[ruleId],
    options:
      ruleId === "no-mix-dearu-desumasu"
        ? { preferInHeader: "", preferInBody: "ですます", preferInList: "ですます", strict: false }
        : technicalWriting.rulesConfig[ruleId]
  }));
  rules.push({
    ruleId: "adocweave-terminology",
    rule: createTerminologyRule(terminology)
  });

  const kernel = new TextlintKernel();
  let violations = 0;
  for (const path of tracked.filter((entry) => classification(entry) === "authored")) {
    const absolute = `${repositoryRoot}${path}`;
    const source = readFileSync(absolute, "utf8");
    const before = createHash("sha256").update(source).digest("hex");
    const result = await kernel.lintText(source, {
      ext: ".adoc",
      filePath: absolute,
      plugins: [{ pluginId: "adocweave", plugin }],
      rules,
      filterRules: [{ ruleId: "comments", rule: commentsFilter }]
    });
    const after = createHash("sha256").update(readFileSync(absolute)).digest("hex");
    if (before !== after) {
      throw new Error(`校正処理が文書を書き換えました: ${path}`);
    }
    for (const message of result.messages) {
      violations += 1;
      console.error(
        `${path}:${message.line}:${message.column}: ${message.message} (${message.ruleId})`
      );
    }
  }
  if (violations > 0) {
    process.exitCode = 1;
  }
}

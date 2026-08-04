import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { TextlintKernel } from "@textlint/kernel";
import commentsFilter from "textlint-filter-rule-comments";

import plugin from "./processor.mjs";
import { classifyTrackedFiles, createRepositoryRules } from "./repository-lint-config.mjs";

const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const targets = JSON.parse(readFileSync(new URL("./targets.json", import.meta.url), "utf8"));
const terminology = JSON.parse(
  readFileSync(new URL("../../config/japanese-terminology.json", import.meta.url), "utf8")
);

const tracked = execFileSync("git", ["ls-files", "-z", "*.adoc"], {
  cwd: repositoryRoot,
  encoding: "utf8"
})
  .split("\0")
  .filter(Boolean)
  .sort();

const classified = classifyTrackedFiles(targets, tracked);
if (classified.unknown.length > 0) {
  console.error(`校正対象が分類されていません。\n${classified.unknown.join("\n")}`);
  process.exitCode = 2;
} else {
  const rules = createRepositoryRules(terminology);

  const kernel = new TextlintKernel();
  let violations = 0;
  for (const path of classified.authored) {
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

import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  assertExpectedDiagnostic,
  npmInvocation,
  npxArguments,
  npxSettings,
  runTextlintPluginNpxSmoke,
} from "./textlint-plugin-npx-smoke.mjs";

const contract = {
  compatibility: { textlintVersion: "15.8.0" },
  identity: {
    packageName: "@adocweave/textlint-plugin-asciidoc",
    pluginName: "@adocweave/asciidoc",
  },
  oneShot: {
    preset: "ja-technical-writing",
    rulePackage: "textlint-rule-preset-ja-technical-writing",
    ruleVersion: "12.0.2",
  },
};

test("candidate tgzを固定したnpx package引数で検査する", async () => {
  const root = await mkdtemp(join(tmpdir(), "adocweave-textlint-npx-test-"));
  const archive = join(root, "plugin.tgz");
  let invocation;
  try {
    await writeFile(archive, "fixture");
    await runTextlintPluginNpxSmoke(archive, {
      contract,
      invokeNpm: async (value) => {
        invocation = value;
        return {
          code: 1,
          stderr: "",
          stdout: JSON.stringify([{
            filePath: "document.adoc",
            messages: [{ line: 3, ruleId: "ja-technical-writing/sentence-length" }],
          }]),
        };
      },
    });
    assert.deepEqual(invocation.args, npxArguments(archive, npxSettings(contract)));
    assert.deepEqual(invocation.args.slice(0, 7), [
      "exec",
      "--yes",
      "--package=textlint@15.8.0",
      `--package=${archive}`,
      "--package=textlint-rule-preset-ja-technical-writing@12.0.2",
      "--",
      "textlint",
    ]);
  } finally {
    await rm(root, { force: true, recursive: true });
  }
});

test("期待する診断がない成功らしい出力を拒否する", () => {
  assert.throws(
    () => assertExpectedDiagnostic(JSON.stringify([{ messages: [] }])),
    /expected sentence-length diagnostic/,
  );
});

test("契約のone-shot設定欠落を拒否する", () => {
  assert.throws(
    () => npxSettings({ ...contract, oneShot: {} }),
    /missing the one-shot execution settings/,
  );
});

test("WindowsではNode.jsからnpm CLIを起動する", () => {
  assert.deepEqual(
    npmInvocation({
      environment: {},
      executable: String.raw`C:\node\node.exe`,
      platform: "win32",
    }),
    {
      arguments: [String.raw`C:\node\node_modules\npm\bin\npm-cli.js`],
      command: String.raw`C:\node\node.exe`,
    },
  );
});

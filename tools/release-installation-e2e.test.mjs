import assert from "node:assert/strict";
import test from "node:test";
import {
  missingInstallationAssets,
  requiredInstallationAssets,
} from "./platform-contract.mjs";
import plan from "../release/distribution-plan.json" with { type: "json" };

const target = "x86_64-unknown-linux-musl";
const version = plan.packageVersion;
const archiveType = "zip";

for (const [scope, expected] of [
  [
    "native-only",
    [
      `adocweave-cli-${target}.zip`,
      `adocweave-lsp-${target}.zip`,
    ],
  ],
  [
    "global-only",
    [
      `adocweave-browser-${version}.tar.xz`,
      `adocweave-textlint-plugin-asciidoc-${version}.tgz`,
      `adocweave-zed-${version}.tar.xz`,
      `adocweave-vscode-${version}.vsix`,
    ],
  ],
]) {
  test(`${scope}は選択された各assetの欠落を拒否する`, () => {
    const required = requiredInstallationAssets(scope, target, version, archiveType);
    assert.deepEqual(required, expected);
    for (const missing of required) {
      const available = required.filter((name) => name !== missing);
      assert.deepEqual(
        missingInstallationAssets(available, required),
        [missing],
        missing,
      );
    }
  });
}

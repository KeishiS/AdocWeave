import assert from "node:assert/strict";
import test from "node:test";

import { configuredServerPath } from "../src/configuration.js";

test("信頼済みworkspaceでは最も具体的なpathを使用します", () => {
  assert.deepEqual(
    configuredServerPath(
      {
        globalValue: "/global/adocweave-lsp",
        workspaceFolderValue: "/folder/adocweave-lsp",
        workspaceValue: "/workspace/adocweave-lsp",
      },
      true,
    ),
    { path: "/folder/adocweave-lsp", workspaceValueIgnored: false },
  );
});

test("未信頼workspaceではworkspace pathを無視します", () => {
  assert.deepEqual(
    configuredServerPath(
      {
        globalValue: "/global/adocweave-lsp",
        workspaceValue: "/untrusted/adocweave-lsp",
      },
      false,
    ),
    { path: "/global/adocweave-lsp", workspaceValueIgnored: true },
  );
});

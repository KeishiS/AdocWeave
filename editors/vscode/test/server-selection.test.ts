import assert from "node:assert/strict";
import test from "node:test";

import { platformForHost } from "../src/platform.js";
import {
  selectServer,
  type SelectionDependencies,
  type SelectionOptions,
} from "../src/server-selection.js";

const platform = platformForHost("linux", "x64");
const options: SelectionOptions = {
  allowDownload: true,
  installer: { storagePath: "/cache", version: "0.14.0" },
  platform,
  version: "0.14.0",
};

function dependencies(overrides: Partial<SelectionDependencies> = {}): SelectionDependencies {
  return {
    findOnPath: async () => undefined,
    findVerifiedCache: async () => undefined,
    installManagedServer: async () => "/cache/adocweave-lsp",
    requireCompatibleServer: async () => undefined,
    ...overrides,
  };
}

test("明示path、PATH、cache、downloadの順で選択します", async () => {
  assert.equal(
    (await selectServer({ ...options, configuredPath: "/explicit/adocweave-lsp" }, dependencies()))
      .source,
    "configured",
  );
  assert.equal(
    (await selectServer(options, dependencies({ findOnPath: async () => "/path/adocweave-lsp" })))
      .source,
    "path",
  );
  assert.equal(
    (
      await selectServer(
        options,
        dependencies({ findVerifiedCache: async () => "/cache/adocweave-lsp" }),
      )
    ).source,
    "managed-cache",
  );
  assert.equal((await selectServer(options, dependencies())).source, "managed-download");
});

test("明示path不一致はfail closed、PATH不一致はmanagedへ進みます", async () => {
  await assert.rejects(
    selectServer(
      { ...options, configuredPath: "/explicit/adocweave-lsp" },
      dependencies({
        requireCompatibleServer: async () => {
          throw new Error("server-version-mismatch");
        },
      }),
    ),
    /server-version-mismatch/,
  );
  const warnings: string[] = [];
  const selected = await selectServer(
    { ...options, warning: (code) => warnings.push(code) },
    dependencies({
      findOnPath: async () => "/path/adocweave-lsp",
      requireCompatibleServer: async (path) => {
        if (path.startsWith("/path")) throw new Error("server-version-mismatch");
      },
    }),
  );
  assert.equal(selected.source, "managed-download");
  assert.deepEqual(warnings, ["path-server-incompatible"]);
});

test("未対応platformでも明示pathとPATHを使用し、downloadは開始しません", async () => {
  assert.equal(
    (
      await selectServer(
        { ...options, configuredPath: "/explicit/adocweave-lsp", platform: undefined },
        dependencies(),
      )
    ).source,
    "configured",
  );
  assert.equal(
    (
      await selectServer(
        { ...options, platform: undefined },
        dependencies({ findOnPath: async () => "/path/adocweave-lsp" }),
      )
    ).source,
    "path",
  );
  let downloads = 0;
  await assert.rejects(
    selectServer(
      { ...options, platform: undefined },
      dependencies({
        installManagedServer: async () => {
          downloads += 1;
          return "/unexpected";
        },
      }),
    ),
    /managed-platform-unsupported/,
  );
  assert.equal(downloads, 0);
});

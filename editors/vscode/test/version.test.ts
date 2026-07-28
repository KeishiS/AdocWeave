import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { probeServerVersion, requireCompatibleServer } from "../src/version.js";

async function executable(source: string): Promise<{ cleanup: () => Promise<void>; path: string }> {
  const directory = await mkdtemp(join(tmpdir(), "adocweave-version-"));
  const path = join(directory, "adocweave-lsp");
  await writeFile(path, `#!/bin/sh\n${source}\n`);
  await chmod(path, 0o755);
  return {
    cleanup: () => rm(directory, { force: true, recursive: true }),
    path,
  };
}

test("version probeはshellを介さず正しいJSONだけを採用します", {
  skip: process.platform === "win32",
}, async () => {
  const server = await executable(
    `printf '%s\\n' '{"name":"adocweave-lsp","packageVersion":"0.15.0"}'`,
  );
  try {
    assert.deepEqual(await probeServerVersion(server.path), {
      name: "adocweave-lsp",
      packageVersion: "0.15.0",
    });
    await assert.doesNotReject(requireCompatibleServer(server.path, "0.15.0"));
    await assert.rejects(requireCompatibleServer(server.path, "9.9.9"), /server-version-mismatch/);
  } finally {
    await server.cleanup();
  }
});

test("version probeは不正応答と実行失敗を拒否します", {
  skip: process.platform === "win32",
}, async () => {
  const malformed = await executable("printf '%s\\n' 'not-json'");
  try {
    await assert.rejects(probeServerVersion(malformed.path), /invalid-json/);
    await assert.rejects(
      probeServerVersion(join(tmpdir(), "missing-adocweave-lsp")),
      /probe-failed/,
    );
  } finally {
    await malformed.cleanup();
  }
});

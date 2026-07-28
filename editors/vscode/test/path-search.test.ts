import assert from "node:assert/strict";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { findOnPath } from "../src/path-search.js";

test("Windowsではshell scriptを候補にせずexeだけを選択します", async () => {
  const directory = await mkdtemp(join(tmpdir(), "adocweave-path-"));
  try {
    await writeFile(join(directory, "adocweave-lsp.cmd"), "@exit /b 0\r\n");
    assert.equal(await findOnPath("adocweave-lsp", directory, "win32"), undefined);
    await writeFile(join(directory, "adocweave-lsp.exe"), "fixture");
    assert.equal(
      await findOnPath("adocweave-lsp", directory, "win32"),
      join(directory, "adocweave-lsp.exe"),
    );
  } finally {
    await rm(directory, { force: true, recursive: true });
  }
});

test("Unixでは実行権限のあるfileだけを選択します", async () => {
  const directory = await mkdtemp(join(tmpdir(), "adocweave-path-"));
  const executable = join(directory, "adocweave-lsp");
  try {
    await writeFile(executable, "fixture");
    await chmod(executable, 0o644);
    assert.equal(await findOnPath("adocweave-lsp", directory, "linux"), undefined);
    await chmod(executable, 0o755);
    assert.equal(await findOnPath("adocweave-lsp", directory, "linux"), executable);
  } finally {
    await rm(directory, { force: true, recursive: true });
  }
});

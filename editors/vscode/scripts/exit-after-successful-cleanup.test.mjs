import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";

test("未解放handleがあっても成功した検証processを終了します", () => {
  const moduleUrl = new URL("./exit-after-successful-cleanup.mjs", import.meta.url).href;
  const result = spawnSync(
    process.execPath,
    [
      "--input-type=module",
      "--eval",
      `
        import { exitAfterSuccessfulCleanup } from ${JSON.stringify(moduleUrl)};
        setInterval(() => {}, 60_000);
        exitAfterSuccessfulCleanup();
      `,
    ],
    {
      encoding: "utf8",
      timeout: 1_000,
    },
  );

  assert.equal(result.error, undefined);
  assert.equal(result.signal, null);
  assert.equal(result.status, 0);
});

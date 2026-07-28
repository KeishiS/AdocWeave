import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { runTests } from "@vscode/test-electron";

const extensionRoot = resolve(fileURLToPath(new URL("../", import.meta.url)));
const repositoryRoot = resolve(extensionRoot, "../..");
const scratch = mkdtempSync(join(tmpdir(), "adocweave-vscode-host-"));
const userData = join(scratch, "user-data");
const sourceServer =
  process.env.ADOCWEAVE_TEST_SERVER ??
  join(
    repositoryRoot,
    "target",
    "debug",
    process.platform === "win32" ? "adocweave-lsp.exe" : "adocweave-lsp",
  );
const server = join(
  scratch,
  process.platform === "win32" ? "adocweave-lsp-test.exe" : "adocweave-lsp-test",
);

try {
  copyFileSync(sourceServer, server);
  if (process.platform !== "win32") chmodSync(server, 0o755);
  mkdirSync(join(userData, "User"), { recursive: true });
  writeFileSync(
    join(userData, "User", "settings.json"),
    `${JSON.stringify({
      "adocweave.server.download": false,
      "adocweave.server.path": server,
    })}\n`,
  );
  await runTests({
    extensionDevelopmentPath: extensionRoot,
    extensionTestsPath: join(extensionRoot, "dist-test", "test", "suite", "index.js"),
    launchArgs: [
      "--disable-extensions",
      "--disable-gpu",
      "--disable-workspace-trust",
      "--user-data-dir",
      userData,
      join(extensionRoot, "test", "fixtures", "adocweave.code-workspace"),
    ],
    version: "1.125.0",
  });
  const processes =
    process.platform === "win32"
      ? execFileSync(
          "powershell.exe",
          [
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Select-Object -ExpandProperty CommandLine",
          ],
          { encoding: "utf8" },
        )
      : execFileSync("ps", ["-eo", "args="], { encoding: "utf8" });
  if (
    processes
      .split(/\r?\n/)
      .some((line) => line.toLocaleLowerCase("en-US").includes(server.toLocaleLowerCase("en-US")))
  ) {
    throw new Error("extension host終了後もLanguage Server processが残っています");
  }
} finally {
  rmSync(scratch, { force: true, recursive: true });
}

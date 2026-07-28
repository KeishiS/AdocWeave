import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

import {
  downloadAndUnzipVSCode,
  resolveCliArgsFromVSCodeExecutablePath,
} from "@vscode/test-electron";
import { unzipSync, zipSync } from "fflate";

if (process.platform === "linux" && process.env.GITHUB_ACTIONS === "true") {
  delete process.env.LD_LIBRARY_PATH;
}

const packageJson = JSON.parse(readFileSync("package.json", "utf8"));
const baseline = resolve("../../target/distrib", `adocweave-vscode-${packageJson.version}.vsix`);
const extensionId = `${packageJson.publisher}.${packageJson.name}`;
const scratch = mkdtempSync(join(tmpdir(), "adocweave-vsix-install-"));
const extensionsDirectory = join(scratch, "extensions");
const userDataDirectory = join(scratch, "user-data");

function fixtureVersion(version) {
  const entries = unzipSync(readFileSync(baseline));
  const manifest = JSON.parse(Buffer.from(entries["extension/package.json"]).toString("utf8"));
  manifest.version = version;
  entries["extension/package.json"] = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
  const vsixManifest = Buffer.from(entries["extension.vsixmanifest"]).toString("utf8");
  entries["extension.vsixmanifest"] = Buffer.from(
    vsixManifest.replace(/(<Identity\b[^>]*\bVersion=")[^"]+(")/, `$1${version}$2`),
  );
  const path = join(scratch, `adocweave-vscode-${version}.vsix`);
  writeFileSync(path, zipSync(entries, { level: 9 }));
  return path;
}

function runCli(baseArguments, arguments_) {
  const [command, ...prefix] = baseArguments;
  const result = spawnSync(
    command,
    [
      ...prefix,
      "--extensions-dir",
      extensionsDirectory,
      "--user-data-dir",
      userDataDirectory,
      ...arguments_,
    ],
    {
      encoding: "utf8",
      shell: process.platform === "win32",
    },
  );
  if (result.status !== 0) {
    throw new Error(result.stderr || result.stdout || `VS Code CLI exited with ${result.status}`);
  }
  return result.stdout;
}

function installedVersion(baseArguments) {
  const listed = runCli(baseArguments, ["--list-extensions", "--show-versions"]);
  return listed
    .split(/\r?\n/)
    .find((line) => line.toLowerCase().startsWith(`${extensionId.toLowerCase()}@`))
    ?.split("@")
    .at(-1);
}

try {
  mkdirSync(extensionsDirectory);
  mkdirSync(userDataDirectory);
  const executable = await downloadAndUnzipVSCode("1.125.0");
  const cli = resolveCliArgsFromVSCodeExecutablePath(executable);
  const [major, minor, patch] = packageJson.version.split(".").map(Number);
  const updateVersion = `${major}.${minor}.${patch + 1}`;
  const update = fixtureVersion(updateVersion);

  runCli(cli, ["--install-extension", baseline]);
  if (installedVersion(cli) !== packageJson.version) throw new Error("VSIX install failed");
  runCli(cli, ["--install-extension", update, "--force"]);
  if (installedVersion(cli) !== updateVersion) throw new Error("VSIX update failed");
  runCli(cli, ["--install-extension", baseline, "--force"]);
  if (installedVersion(cli) !== packageJson.version) throw new Error("VSIX rollback failed");
  runCli(cli, ["--uninstall-extension", extensionId]);
  if (installedVersion(cli) !== undefined) throw new Error("VSIX uninstall failed");
  if (
    readFileSync(baseline).byteLength === 0 ||
    Object.keys(unzipSync(readFileSync(baseline))).length === 0
  ) {
    throw new Error("baseline VSIX was modified");
  }
  process.stdout.write("VSIXの導入、更新、rollbackおよび削除に成功しました。\n");
} finally {
  rmSync(scratch, { force: true, recursive: true });
}

import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import {
  TEMPORARY_DIRECTORY_REMOVAL_OPTIONS,
  archiveEntries,
  createRuntimeAdapters,
  executableNames,
  importedWindowsDlls,
  installationLayout,
  isPathInside,
  macosMinimumVersion,
  pathImplementation,
  shouldRetryRemoval,
  unexpectedMacosDependencies,
  unexpectedWindowsDlls,
  validateArchiveEntries,
  vscodePackageContract,
} from "./platform-contract.mjs";

test("Windowsの実行ファイル名とインストール先を計算する", () => {
  const pathApi = pathImplementation("win32");
  assert.equal(pathApi, path.win32);
  assert.deepEqual(executableNames(".exe"), ["adocweave.exe", "adocweave-lsp.exe"]);
  assert.deepEqual(installationLayout("C:\\Users\\tester\\.local", "0.16.0", pathApi), {
    binDirectory: "C:\\Users\\tester\\.local\\bin",
    versionRoot: "C:\\Users\\tester\\.local\\lib\\adocweave\\0.16.0",
    currentLink: "C:\\Users\\tester\\.local\\lib\\adocweave\\current",
    activeMarker: "C:\\Users\\tester\\.local\\lib\\adocweave\\active-version",
    browserRoot: "C:\\Users\\tester\\.local\\share\\adocweave\\0.16.0\\browser",
    zedRoot: "C:\\Users\\tester\\.local\\share\\adocweave\\0.16.0\\zed",
    vscodeRoot: "C:\\Users\\tester\\.local\\share\\adocweave\\0.16.0\\vscode",
  });
});

test("macOSの実行ファイル名とインストール先を計算する", () => {
  const pathApi = pathImplementation("darwin");
  assert.equal(pathApi, path.posix);
  assert.deepEqual(executableNames(""), ["adocweave", "adocweave-lsp"]);
  assert.equal(
    installationLayout("/Users/tester/.local", "0.16.0", pathApi).versionRoot,
    "/Users/tester/.local/lib/adocweave/0.16.0",
  );
});

test("WindowsとPOSIXのpath境界をそれぞれ判定する", () => {
  assert.equal(isPathInside("C:\\release", "C:\\release\\bin\\adocweave.exe", path.win32), true);
  assert.equal(isPathInside("C:\\release", "C:\\release-other\\file", path.win32), false);
  assert.equal(isPathInside("/release", "/release/bin/adocweave", path.posix), true);
  assert.equal(isPathInside("/release", "/release-other/file", path.posix), false);
});

test("CRLFのZIP一覧を正規化し、VSIXを含むarchive pathを検査する", () => {
  const entries = archiveEntries("extension/package.json\r\nextension/dist/extension.cjs\r\n");
  assert.deepEqual(entries, ["extension/package.json", "extension/dist/extension.cjs"]);
  assert.deepEqual(validateArchiveEntries(entries, "extension"), []);
  assert.deepEqual(validateArchiveEntries(["extension\\package.json", "../escape"], "extension"), [
    "extension\\package.json",
    "../escape",
  ]);
});

test("Windows DLLのsystem allowlistとAPI setだけを許可する", () => {
  const imported = importedWindowsDlls(`
    KERNEL32.dll
    api-ms-win-core-file-l1-1-0.dll
    ext-ms-win-ntuser-window-l1-1-0.dll
    third-party.dll
  `);
  assert.deepEqual(unexpectedWindowsDlls(imported), ["third-party.dll"]);
});

test("macOS archiveのdependencyとdeployment targetを検査する", () => {
  const dependencies = "adocweave:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n" +
    "\t/System/Library/Frameworks/Security.framework/Versions/A/Security (compatibility version 1.0.0)\n";
  assert.deepEqual(unexpectedMacosDependencies(dependencies), []);
  assert.deepEqual(unexpectedMacosDependencies(`${dependencies}\t/opt/local/lib/libextra.dylib\n`), [
    "/opt/local/lib/libextra.dylib",
  ]);
  assert.equal(macosMinimumVersion("cmd LC_BUILD_VERSION\n  cmdsize 32\n platform 1\n    minos 14.0\n"), "14.0");
});

test("Windowsの一時directory削除だけを規定回数再試行する", () => {
  const busy = Object.assign(new Error("busy"), { code: "EBUSY" });
  assert.equal(shouldRetryRemoval(busy, "win32"), true);
  assert.equal(shouldRetryRemoval(busy, "darwin"), false);
  assert.deepEqual(TEMPORARY_DIRECTORY_REMOVAL_OPTIONS, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 100,
  });
});

test("VSIXのmanifest metadataを検査する", () => {
  assert.equal(vscodePackageContract({ version: "0.16.0", main: "./dist/extension.cjs" }, "0.16.0"), true);
  assert.equal(vscodePackageContract({ version: "0.16.0", main: "./src/extension.js" }, "0.16.0"), false);
});

test("filesystem・process・時刻・platformをruntime adapterとして注入する", () => {
  const fileSystem = { readFile() {} };
  const processControl = { spawn() {} };
  const time = { now: () => 42 };
  const platform = { os: "win32", architecture: "x64", environment: {} };
  const adapters = createRuntimeAdapters({
    fileSystem,
    processControl,
    time,
    platform,
    pathApi: path.win32,
  });
  assert.equal(adapters.fileSystem, fileSystem);
  assert.equal(adapters.processControl, processControl);
  assert.equal(adapters.time.now(), 42);
  assert.equal(adapters.platform, platform);
  assert.equal(adapters.pathApi, path.win32);
  assert.throws(
    () => createRuntimeAdapters({ fileSystem, processControl, time, platform: {}, pathApi: path.win32 }),
    /requires os and architecture/,
  );
});
